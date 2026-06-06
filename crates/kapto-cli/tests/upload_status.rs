// SPDX-License-Identifier: Apache-2.0

//! `kapto upload status` and `kapto upload retry`: where uploads are, read
//! from `kapto.db`, and failed ones tried again now, asked of serve.

#![cfg(unix)]
#![allow(
    clippy::unwrap_used,
    reason = "test helpers fail the test loudly when the setup itself breaks"
)]

use std::path::Path;
use std::process::{Command, Output};

use kapto_server::jobs::{JOB_SOCKET, JobServer, UPLOAD};
use kapto_store::db::DATABASE_FILE;
use kapto_store::{JobFailure, JobState, UploadCounts, Writer};
use kapto_testkit::scratch;

const DONE: &str = "20260310T120000.000Z-00000001";
const GOING: &str = "20260310T120100.000Z-00000002";
const FAILED: &str = "20260310T120200.000Z-00000003";

/// `kapto upload` with `args`, with the destinations `site` and `cloud`
/// and the data directory in `folder`.
fn upload(folder: &Path, args: &[&str]) -> Output {
    let config = folder.join("kapto.toml");
    std::fs::write(
        &config,
        "[[upload.destination]]\nname = \"site\"\nurl = \"s3://site-data\"\n\
         [[upload.destination]]\nname = \"cloud\"\nurl = \"gs://cloud-data\"\n",
    )
    .unwrap();
    Command::new(env!("CARGO_BIN_EXE_kapto"))
        .arg("--config")
        .arg(&config)
        .arg("--data-dir")
        .arg(folder.join("data"))
        .arg("upload")
        .args(args)
        .env_clear()
        .output()
        .unwrap()
}

/// `kapto.db` with one upload done, one going and one failed, all to `site`.
fn uploads(folder: &Path) -> Writer {
    let data = folder.join("data");
    std::fs::create_dir_all(&data).unwrap();
    let store = Writer::open(&data.join(DATABASE_FILE)).unwrap();
    for episode in [DONE, GOING, FAILED] {
        store.start_upload(episode, "site", episode).unwrap();
    }
    store.uploaded(DONE, "site", Vec::new()).unwrap();
    let counts = UploadCounts {
        bytes_sent: 3 << 20,
        bytes_total: 12 << 20,
        throttled: 2,
    };
    store.upload_progress(GOING, "site", "{}", counts).unwrap();
    let failure = JobFailure {
        code: "upload.unreachable".into(),
        message: "connection reset".into(),
    };
    store.upload_failed(FAILED, "site", failure).unwrap();
    store
}

#[test]
fn the_status_says_how_each_destination_and_each_upload_not_done_is_going() {
    let folder = scratch("upload-status");
    let _store = uploads(&folder);
    let output = upload(&folder, &["status"]);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "{stdout}{}",
        String::from_utf8_lossy(&output.stderr)
    );
    for line in [
        "site (s3://site-data): 1 uploaded, 1 uploading, 1 failed, 2 requests throttled",
        "cloud (gs://cloud-data): 0 uploaded, 0 uploading, 0 failed",
        &format!("  {GOING} to site: 3.0 MiB of 12.0 MiB"),
        &format!("  {FAILED} to site: failed, error[upload.unreachable]: connection reset"),
        "0 episodes waiting to upload",
    ] {
        assert!(stdout.contains(line), "{line:?} not in:\n{stdout}");
    }
    assert!(
        !stdout.contains(DONE),
        "only those not done are listed:\n{stdout}"
    );
}

#[test]
fn a_failed_upload_is_queued_again_at_once_through_serve_and_nothing_else_is() {
    let folder = scratch("upload-retry");
    // Without serve, it says what it needs.
    let output = upload(&folder, &["retry", FAILED]);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.status.code(), Some(1), "{stderr}");
    assert!(
        stderr.contains("error[worker.serve_unreachable]") && stderr.contains("kapto serve"),
        "{stderr}"
    );

    let store = uploads(&folder);
    let _serve = JobServer::listen(&folder.join("data").join(JOB_SOCKET), store.clone()).unwrap();
    let output = upload(&folder, &["retry", FAILED]);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "{stdout}{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        stdout.contains(&format!("{FAILED}: queued to try again now, to site")),
        "{stdout}"
    );
    let reader = store.reader().unwrap();
    assert_eq!(
        reader.job(UPLOAD, FAILED).unwrap().unwrap().state,
        JobState::Queued
    );
    assert_eq!(
        reader
            .upload(FAILED, "site")
            .unwrap()
            .unwrap()
            .failed_utc_ns,
        Some(0),
        "due now"
    );

    // One that didn't fail has nothing to try again.
    let output = upload(&folder, &["retry", GOING]);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.status.code(), Some(1), "{stderr}");
    assert!(
        stderr.contains("error[upload.nothing_to_retry]"),
        "{stderr}"
    );
}
