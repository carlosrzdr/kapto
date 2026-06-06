// SPDX-License-Identifier: Apache-2.0

//! Uploads one episode, a recording of `--mib` MiB and its sidecars made
//! in `--folder` if it isn't there, to the RustFS server `cargo xtask bench
//! upload` starts, with `--part` MiB parts and `--parallel` of them at once
//! (the defaults if not given), and prints a line of JSON: how long it took
//! and the CPU and peak memory it used. Each upload is a process of its
//! own, so what one used is never counted in another's. It goes as the
//! worker's does: its destination resolved from `kapto.toml`'s text, its
//! store opened and its policy made by the engine's own functions, the
//! marker's checksums worked out first. Run by `cargo xtask bench upload`
//! for each setting; not a Criterion benchmark, as one upload of hundreds
//! of MiB is already the measurement.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::cast_precision_loss,
    reason = "a benchmark fails loudly, and shows counts as fractions"
)]

use std::path::Path;
use std::time::{Duration, Instant};

use futures::TryStreamExt as _;
use kapto_config::{ConfigFile, DeploymentConfig, Inputs};
use kapto_core::EpisodeId;
use kapto_store::manifest::{MANIFEST_FILE, Manifest};
use kapto_upload::backoff::Backoff;
use kapto_upload::episode::{Progress, RuntimeSleep, marker_of, upload_episode};
use kapto_upload::{rustfs, stores};
use object_store::ObjectStoreExt as _;
use object_store::path::Path as Key;

/// The value after `--name` on the command line, if given.
fn arg(name: &str) -> Option<String> {
    std::env::args().skip_while(|arg| arg != name).nth(1)
}

fn main() {
    let number = |name: &str| arg(name).map(|value| value.parse::<u64>().expect(name));
    let folder = std::path::PathBuf::from(arg("--folder").expect("--folder"));
    let mib = number("--mib").unwrap_or(512);
    let run =
        number("--part").zip(number("--parallel").map(|parallel| u32::try_from(parallel).unwrap()));
    let key = arg("--key").unwrap_or_else(|| "run".to_owned());
    if !folder.join(MANIFEST_FILE).exists() {
        episode(&folder, mib);
    }
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();
    let line = runtime.block_on(async {
        rustfs::bucket("bench").await;
        upload(&folder, &key, run).await
    });
    println!("{line}");
}

/// An episode of one recording of `mib` MiB, its bytes all different by
/// place, and a small sidecar, with its manifest; written a MiB at a time,
/// so making it takes little memory.
fn episode(folder: &Path, mib: u64) {
    use std::io::Write as _;
    std::fs::create_dir_all(folder).unwrap();
    let mebibyte: Vec<u8> = (0..1 << 20)
        .map(|at| u8::try_from(at % 251).unwrap())
        .collect();
    let mut recording = std::fs::File::create(folder.join("recording.mcap")).unwrap();
    for _ in 0..mib {
        recording.write_all(&mebibyte).unwrap();
    }
    recording.sync_all().unwrap();
    std::fs::write(folder.join("episode.json"), b"{}\n").unwrap();
    let id: EpisodeId = "20260325T120000.000Z-0000be4c".parse().unwrap();
    Manifest::of_directory(folder, &id)
        .unwrap()
        .write(folder)
        .unwrap();
}

/// The destination as `kapto.toml` would declare it, with `run`'s part
/// size and parts at once; the defaults for `None`.
fn kapto_toml(run: Option<(u64, u32)>) -> String {
    let settings = run.map_or_else(String::new, |(part, parallel)| {
        format!("part_size = {part}\nparallel_parts = {parallel}\nmax_in_flight = {parallel}\n")
    });
    format!(
        "[upload]\n{settings}\n[[upload.destination]]\nname = \"bench\"\nurl = \"s3://bench\"\n\
         endpoint = \"{}\"\nregion = \"us-east-1\"\n",
        rustfs::endpoint()
    )
}

async fn upload(folder: &Path, key: &str, run: Option<(u64, u32)>) -> String {
    let file = ConfigFile {
        path: "kapto.toml".into(),
        contents: kapto_toml(run),
    };
    let env = |name: &str| std::env::var(name).ok();
    let config = DeploymentConfig::resolve(&Inputs {
        file: Some(&file),
        env: &env,
        cli: &[],
    })
    .unwrap();
    let destination = &config.upload_destinations[0];
    let settings = &destination.settings;
    let opened = stores::open(destination, &env).unwrap();
    let policy = stores::policy(settings, &opened);
    let key = Key::from(key);
    let mut backoff =
        Backoff::from_clock(settings.backoff_initial.value, settings.backoff_max.value);

    let (cpu_before, started) = (cpu_seconds(), Instant::now());
    let marker = marker_of(folder, Some("arm7".into()), 0).unwrap();
    let bytes: u64 = marker.files.iter().map(|file| file.bytes).sum();
    upload_episode(
        opened.store(),
        folder,
        &key,
        marker,
        &policy,
        &mut backoff,
        &RuntimeSleep,
        &mut Progress::default(),
        &mut |_: &Progress| {},
        &mut || Ok(()),
    )
    .await
    .unwrap();
    let seconds = started.elapsed().as_secs_f64();
    let cpu = cpu_seconds() - cpu_before;
    let peak_memory_mib = peak_memory_kib() as f64 / 1024.0;

    // Gone before the next run, so the server's disk holds one at a time.
    let objects = opened.store().objects;
    let listed: Vec<_> = objects.list(Some(&key)).try_collect().await.unwrap();
    for object in listed {
        objects.delete(&object.location).await.unwrap();
    }
    serde_json::json!({
        "part_size_mib": settings.part_size_mib.value,
        "parallel_parts": settings.parallel_parts.value,
        "max_in_flight": settings.max_in_flight.value,
        "defaults": run.is_none(),
        "bytes": bytes,
        "seconds": seconds,
        "cpu_cores": cpu / seconds.max(f64::EPSILON),
        "peak_memory_mib": peak_memory_mib,
    })
    .to_string()
}

/// This process's user and system CPU time so far, from `/proc`.
fn cpu_seconds() -> f64 {
    let stat = std::fs::read_to_string("/proc/self/stat").unwrap();
    let fields: Vec<&str> = stat[stat.rfind(')').unwrap() + 1..]
        .split_whitespace()
        .collect();
    let ticks: u64 = fields[11].parse::<u64>().unwrap() + fields[12].parse::<u64>().unwrap();
    // Linux counts it in ticks of a hundredth of a second (USER_HZ).
    Duration::from_millis(ticks * 10).as_secs_f64()
}

fn peak_memory_kib() -> u64 {
    let status = std::fs::read_to_string("/proc/self/status").unwrap();
    status
        .lines()
        .find_map(|line| line.strip_prefix("VmHWM:"))
        .and_then(|value| value.split_whitespace().next()?.parse().ok())
        .unwrap()
}
