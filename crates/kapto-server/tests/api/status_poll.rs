// SPDX-License-Identifier: Apache-2.0

//! Capture's status, asked for by one poller and read by every request.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use axum::http::StatusCode;
use kapto_capture::control::{PROTOCOL_VERSION, Request, Response, Status};
use kapto_server::api::ApiState;
use kapto_testkit::scratch;

use crate::common::{FakeCapture, call, recording, state};

/// How often the poller asks capture.
const POLL_MS: u64 = 250;

/// A capture process that counts the connections made to it and the
/// status requests it answers, and answers with the milliseconds since it
/// started in `seconds`, so a reader can tell when its answer was made.
struct CountingCapture {
    counts: Arc<Counts>,
}

struct Counts {
    started: Instant,
    connections: AtomicU64,
    statuses: AtomicU64,
    recording: AtomicBool,
}

impl Counts {
    fn now_ms(&self) -> u64 {
        u64::try_from(self.started.elapsed().as_millis()).unwrap()
    }
}

impl CountingCapture {
    fn start(socket: &Path) -> Self {
        let listener = UnixListener::bind(socket).unwrap();
        let counts = Arc::new(Counts {
            started: Instant::now(),
            connections: AtomicU64::new(0),
            statuses: AtomicU64::new(0),
            recording: AtomicBool::new(false),
        });
        thread::spawn({
            let counts = Arc::clone(&counts);
            move || {
                for connection in listener.incoming() {
                    counts.connections.fetch_add(1, Ordering::SeqCst);
                    let counts = Arc::clone(&counts);
                    thread::spawn(move || converse(connection.unwrap(), &counts));
                }
            }
        });
        Self { counts }
    }

    fn connections(&self) -> u64 {
        self.counts.connections.load(Ordering::SeqCst)
    }

    fn statuses(&self) -> u64 {
        self.counts.statuses.load(Ordering::SeqCst)
    }
}

fn converse(connection: UnixStream, counts: &Counts) {
    let mut writer = connection.try_clone().unwrap();
    for line in BufReader::new(connection).lines() {
        let Ok(line) = line else { return };
        let response = match serde_json::from_str::<Request>(&line).unwrap() {
            Request::Hello { .. } => Response::Hello {
                version: PROTOCOL_VERSION,
                kapto: "test".into(),
            },
            Request::Status => {
                counts.statuses.fetch_add(1, Ordering::SeqCst);
                Response::Status(Status {
                    episode: counts
                        .recording
                        .load(Ordering::SeqCst)
                        .then(|| "20260310T120000.000Z-1a2b3c4d".into()),
                    seconds: counts.now_ms(),
                    ..Status::default()
                })
            }
            Request::Start { .. } => {
                counts.recording.store(true, Ordering::SeqCst);
                Response::Started {
                    episode: "20260310T120000.000Z-1a2b3c4d".into(),
                }
            }
            other => panic!("not expected here: {other:?}"),
        };
        let text = serde_json::to_string(&response).unwrap();
        if writeln!(writer, "{text}").is_err() {
            return;
        }
    }
}

async fn read_status(state: &ApiState) -> (StatusCode, serde_json::Value) {
    call(state.clone(), "GET", "/api/status", None).await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn many_pages_reading_the_status_cost_capture_one_connection_and_a_fixed_rate() {
    let folder = scratch("status-shared");
    let capture = CountingCapture::start(&folder.join("capture.sock"));
    let state = state(&folder);
    let began = Instant::now();
    let readers: Vec<_> = (0..16)
        .map(|_| {
            let state = state.clone();
            tokio::spawn(async move {
                for _ in 0..10 {
                    assert_eq!(read_status(&state).await.0, StatusCode::OK);
                    tokio::time::sleep(Duration::from_millis(50)).await;
                }
            })
        })
        .collect();
    for reader in readers {
        reader.await.unwrap();
    }
    let elapsed = u64::try_from(began.elapsed().as_millis()).unwrap();
    // 160 reads, one connection, and no more requests than the poller's
    // rate allows over the time they took.
    assert_eq!(capture.connections(), 1);
    let most = elapsed / POLL_MS + 2;
    assert!(
        capture.statuses() <= most,
        "{} status requests in {elapsed} ms",
        capture.statuses()
    );
}

#[tokio::test]
async fn every_status_read_was_asked_of_capture_within_the_last_poll() {
    let folder = scratch("status-fresh");
    let capture = CountingCapture::start(&folder.join("capture.sock"));
    let state = state(&folder);
    for _ in 0..12 {
        let read = capture.counts.now_ms();
        let (status, body) = read_status(&state).await;
        assert_eq!(status, StatusCode::OK);
        let answered = body["capture"]["seconds"].as_u64().unwrap();
        assert!(
            answered + POLL_MS >= read,
            "read at {read} ms, answered at {answered} ms"
        );
        tokio::time::sleep(Duration::from_millis(90)).await;
    }
}

#[tokio::test]
async fn a_status_read_once_capture_has_gone_says_it_is_unreachable_with_its_cause() {
    let folder = scratch("status-gone");
    let capture = FakeCapture::start(&folder.join("capture.sock"), recording);
    let state = state(&folder);
    assert_eq!(read_status(&state).await.0, StatusCode::OK);
    drop(capture);
    // Past what the poller had asked before capture went.
    tokio::time::sleep(Duration::from_millis(POLL_MS + 50)).await;
    let (status, body) = read_status(&state).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(body["code"], "capture.control_unreachable");
}

#[tokio::test]
async fn a_status_read_right_after_an_order_sees_what_the_order_changed() {
    let folder = scratch("status-after-order");
    let _capture = CountingCapture::start(&folder.join("capture.sock"));
    let state = state(&folder);
    let (_, body) = read_status(&state).await;
    assert_eq!(body["capture"]["episode"], serde_json::Value::Null);
    let (status, _) = call(
        state.clone(),
        "POST",
        "/api/episodes/start",
        Some(("application/json", "{}")),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    let (_, body) = read_status(&state).await;
    assert_eq!(body["capture"]["episode"], "20260310T120000.000Z-1a2b3c4d");
}
