// SPDX-License-Identifier: Apache-2.0

//! The network to S3-compatible storage cut in the middle of an upload,
//! and restored: through a proxy in front of the RustFS server
//! `cargo xtask storage` starts, which drops every connection and refuses
//! new ones while the link is down. A short cut is waited out with the
//! configured backoff and the same upload carries on; a cut longer than the
//! tries allow fails the upload, which then carries on from its progress
//! once the link is back, the parts already up not sent again.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "a test's setup fails loudly, saying what is missing"
)]

use std::future::Future;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use kapto_core::{EpisodeId, ErrorCode};
use kapto_store::manifest::Manifest;
use object_store::aws::{AmazonS3, AmazonS3Builder, Checksum};
use object_store::path::Path;
use object_store::{ClientOptions, ObjectStoreExt as _, RetryConfig};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::watch;

use crate::backoff::Backoff;
use crate::episode::{
    FileProgress, Policy, Progress, RuntimeSleep, Sleep, Store, Uploaded, Verify, marker_of,
    upload_episode,
};
use crate::rustfs;

const MIB: usize = 1 << 20;
/// Four parts of the smallest S3 takes, the last one shorter.
const RECORDING: usize = 18 * MIB;
const PART: usize = 5 << 20;
const INITIAL: Duration = Duration::from_millis(40);
const MAX: Duration = Duration::from_millis(320);
const SEED: u64 = 7;

/// A link to the server that can be cut: what is sent through it is
/// counted, and while it is down every connection is dropped and new ones
/// are closed as soon as they are made.
struct Link {
    address: SocketAddr,
    up: watch::Sender<bool>,
    sent: Arc<AtomicU64>,
}

impl Link {
    async fn to(server: SocketAddr) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let (up, watching) = watch::channel(true);
        let sent = Arc::new(AtomicU64::new(0));
        let counted = Arc::clone(&sent);
        tokio::spawn(async move {
            while let Ok((client, _)) = listener.accept().await {
                let watching = watching.clone();
                if !*watching.borrow() {
                    continue;
                }
                tokio::spawn(forward(client, server, watching, Arc::clone(&counted)));
            }
        });
        Self { address, up, sent }
    }

    fn sent(&self) -> u64 {
        self.sent.load(Ordering::Relaxed)
    }

    fn set(&self, up: bool) {
        self.up.send_replace(up);
    }

    /// Cuts the link once `after` bytes have gone through it, for `cut`,
    /// or until it is set up again if `None`.
    fn cut_after(self: &Arc<Self>, after: u64, cut: Option<Duration>) {
        let link = Arc::clone(self);
        tokio::spawn(async move {
            while link.sent() < after {
                tokio::time::sleep(Duration::from_millis(2)).await;
            }
            link.set(false);
            if let Some(cut) = cut {
                tokio::time::sleep(cut).await;
                link.set(true);
            }
        });
    }
}

/// Passes one connection on to `server` until either side ends it or the
/// link goes down, counting the bytes the client sends.
async fn forward(
    client: TcpStream,
    server: SocketAddr,
    mut watching: watch::Receiver<bool>,
    sent: Arc<AtomicU64>,
) {
    let Ok(upstream) = TcpStream::connect(server).await else {
        return;
    };
    let (mut from_client, mut to_client) = client.into_split();
    let (mut from_server, mut to_server) = upstream.into_split();
    let up = async move {
        let mut buffer = vec![0; 64 * 1024];
        loop {
            let read = from_client.read(&mut buffer).await?;
            if read == 0 {
                return Ok::<_, std::io::Error>(());
            }
            sent.fetch_add(read as u64, Ordering::Relaxed);
            to_server.write_all(&buffer[..read]).await?;
        }
    };
    let down = tokio::io::copy(&mut from_server, &mut to_client);
    tokio::select! {
        _ = up => {}
        _ = down => {}
        _ = watching.wait_for(|up| !*up) => {}
    }
}

/// Every wait the engine asks for, waited on the runtime's timer.
#[derive(Default)]
struct Recorded(Mutex<Vec<Duration>>);

impl Sleep for Recorded {
    fn sleep(&self, time: Duration) -> Pin<Box<dyn Future<Output = ()> + Send + '_>> {
        self.0.lock().unwrap().push(time);
        RuntimeSleep.sleep(time)
    }

    fn now(&self) -> Duration {
        RuntimeSleep.now()
    }
}

/// An episode of one large recording, its bytes all different by place.
fn episode(test: &str) -> (std::path::PathBuf, Vec<u8>) {
    let folder = kapto_testkit::scratch(test);
    let recording: Vec<u8> = (0..RECORDING)
        .map(|at| u8::try_from(at % 251).unwrap())
        .collect();
    std::fs::write(folder.join("recording.mcap"), &recording).unwrap();
    std::fs::write(folder.join("episode.json"), b"{}\n").unwrap();
    let id: EpisodeId = "20260325T120000.000Z-0000c0de".parse().unwrap();
    Manifest::of_directory(&folder, &id)
        .unwrap()
        .write(&folder)
        .unwrap();
    (folder, recording)
}

/// The bucket `name` as the engine's S3 stores reach it, through `link`:
/// checking each file's SHA-256, never trying a request again by itself.
async fn through(link: &Link, name: &str) -> AmazonS3 {
    rustfs::bucket(name).await;
    AmazonS3Builder::from_env()
        .with_endpoint(format!("http://{}", link.address))
        .with_bucket_name(name)
        .with_checksum_algorithm(Checksum::SHA256)
        .with_client_options(
            ClientOptions::new()
                .with_allow_http(true)
                .with_connect_timeout(Duration::from_secs(2))
                .with_timeout(Duration::from_secs(10)),
        )
        .with_retry(RetryConfig {
            max_retries: 0,
            retry_timeout: Duration::ZERO,
            ..RetryConfig::default()
        })
        .build()
        .unwrap()
}

/// The server, as `cargo xtask storage` gives it.
fn server() -> SocketAddr {
    let endpoint = rustfs::endpoint();
    endpoint
        .trim_start_matches("http://")
        .parse()
        .expect("the server's address")
}

fn policy(max_attempts: u32) -> Policy {
    Policy {
        part_bytes: PART,
        most_parts: 10_000,
        parallel_parts: 2,
        max_in_flight: 2,
        max_attempts,
        verify: Verify::ServerChecksum,
        store_checks_sha256: true,
        marker: true,
        bandwidth: None,
    }
}

async fn upload(
    store: &AmazonS3,
    folder: &std::path::Path,
    policy: &Policy,
    backoff: &mut Backoff,
    sleep: &Recorded,
    progress: &mut Progress,
) -> Result<Uploaded, kapto_core::KaptoError> {
    upload_episode(
        Store {
            objects: store,
            multipart: Some(store),
        },
        folder,
        &Path::from("arm7/episode"),
        marker_of(folder, Some("arm7".into()), 7).unwrap(),
        policy,
        backoff,
        sleep,
        progress,
        &mut |_: &Progress| {},
        &mut || Ok(()),
    )
    .await
}

/// The recording as it is at the destination.
async fn uploaded(name: &str) -> Vec<u8> {
    let direct = rustfs::bucket(name).await;
    let at = Path::from("arm7/episode/recording.mcap");
    direct
        .get(&at)
        .await
        .unwrap()
        .bytes()
        .await
        .unwrap()
        .to_vec()
}

#[tokio::test]
#[ignore = "needs the RustFS server `cargo xtask storage` starts"]
async fn s3_upload_cut_mid_file_waits_with_the_backoff_and_carries_on_once_the_link_is_back() {
    let (folder, recording) = episode("network-cut-short");
    let link = Arc::new(Link::to(server()).await);
    let store = through(&link, "network-cut-short").await;
    let cut = Duration::from_millis(1500);
    link.cut_after(7 << 20, Some(cut));
    let (sleep, mut progress) = (Recorded::default(), Progress::default());
    let mut backoff = Backoff::new(INITIAL, MAX, SEED);

    let outcome = upload(
        &store,
        &folder,
        &policy(40),
        &mut backoff,
        &sleep,
        &mut progress,
    )
    .await;

    assert!(matches!(outcome, Ok(Uploaded::Now(_))), "{outcome:?}");
    assert!(progress.done);
    assert!(
        uploaded("network-cut-short").await == recording,
        "the recording differs"
    );
    // Each wait is the configured backoff's next draw for some retry: up
    // to the first wait doubled that many times, never past the maximum.
    let waits = sleep.0.into_inner().unwrap();
    assert!(waits.len() >= 2, "the cut was waited out: {waits:?}");
    let mut replay = Backoff::new(INITIAL, MAX, SEED);
    for (n, wait) in waits.iter().enumerate() {
        let drawn: Vec<Duration> = (0..16).map(|retry| replay.clone().wait(retry)).collect();
        assert!(
            drawn.contains(wait),
            "wait {n}, {wait:?}, isn't one of {drawn:?}"
        );
        assert!(*wait <= MAX);
        replay.wait(0);
    }
    // Waiting, not trying over and over: the waits span most of the cut,
    // the engine's requests being cut off at once while it lasts.
    let waited: Duration = waits.iter().sum();
    assert!(
        waited >= cut / 2,
        "waited {waited:?} through a cut of {cut:?}"
    );
}

#[tokio::test]
#[ignore = "needs the RustFS server `cargo xtask storage` starts"]
async fn s3_upload_cut_longer_than_its_tries_carries_on_from_its_progress_once_the_link_is_back() {
    let (folder, recording) = episode("network-cut-long");
    let link = Arc::new(Link::to(server()).await);
    let store = through(&link, "network-cut-long").await;
    // Cut while the third and fourth parts go up, and kept down.
    link.cut_after(12 << 20, None);
    let (sleep, mut progress) = (Recorded::default(), Progress::default());
    let mut backoff = Backoff::new(INITIAL, MAX, SEED);

    let failed = upload(
        &store,
        &folder,
        &policy(3),
        &mut backoff,
        &sleep,
        &mut progress,
    )
    .await;

    let error = failed.expect_err("the link stayed down");
    assert_eq!(error.code(), ErrorCode::UploadUnreachable, "{error}");
    let Some(FileProgress::Parts { parts, .. }) = progress.files.get("recording.mcap") else {
        panic!("the recording's parts aren't kept: {progress:?}");
    };
    let up = parts.len();
    assert!(up >= 1, "{up} parts up before the cut");

    link.set(true);
    let before = link.sent();
    let outcome = upload(
        &store,
        &folder,
        &policy(3),
        &mut backoff,
        &sleep,
        &mut progress,
    )
    .await;

    assert!(matches!(outcome, Ok(Uploaded::Now(_))), "{outcome:?}");
    assert!(
        uploaded("network-cut-long").await == recording,
        "the recording differs"
    );
    let resent = link.sent() - before;
    let left = (RECORDING - up * PART) as u64;
    assert!(
        resent < left + MIB as u64,
        "{resent} bytes sent to finish, for {left} left after {up} parts"
    );
}
