// SPDX-License-Identifier: Apache-2.0

//! `kapto episode delete` and `kapto episode restore`: an episode deleted by
//! hand into the bin and put back, asked of serve.

#![cfg(unix)]
#![allow(
    clippy::unwrap_used,
    reason = "test helpers fail the test loudly when the setup itself breaks"
)]

use std::path::Path;
use std::process::{Command, Output};
use std::time::Duration;

use kapto_server::jobs::{Bin, JOB_SOCKET, JobServer};
use kapto_store::Writer;
use kapto_store::db::DATABASE_FILE;
use kapto_store::manifest::FINALISED_FILE;
use kapto_testkit::scratch;

const EPISODE: &str = "20260310T120000.000Z-00000001";

/// `kapto episode` with `args`, the data directory in `folder`.
fn episode(folder: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_kapto"))
        .arg("--data-dir")
        .arg(folder.join("data"))
        .arg("episode")
        .args(args)
        .env_clear()
        .output()
        .unwrap()
}

#[test]
fn an_episode_is_deleted_into_the_bin_and_put_back_from_the_command_line() {
    let folder = scratch("cli-bin");
    // Without serve, it says what it needs.
    let output = episode(&folder, &["delete", EPISODE]);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.status.code(), Some(1), "{stderr}");
    assert!(stderr.contains("kapto serve"), "{stderr}");

    let data = folder.join("data");
    let recorded = kapto_testkit::episode(
        EPISODE,
        (1, 2),
        vec![kapto_testkit::stream("arm_joints", 100)],
    );
    std::fs::create_dir_all(data.join(EPISODE)).unwrap();
    recorded.write(&data.join(EPISODE)).unwrap();
    std::fs::write(data.join(EPISODE).join(FINALISED_FILE), b"").unwrap();
    let store = Writer::open(&data.join(DATABASE_FILE)).unwrap();
    store.index(recorded.into()).unwrap();
    let serve = JobServer::listen(&data.join(JOB_SOCKET), store.clone()).unwrap();
    serve.empty_bin_by(Bin {
        data_dir: data.clone(),
        keep: Duration::from_hours(7 * 24),
        free_below_bytes: 0,
    });

    let output = episode(&folder, &["delete", EPISODE]);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "{stdout}{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(stdout.contains("in the bin"), "{stdout}");
    assert!(
        data.join(".bin")
            .join(EPISODE)
            .join(FINALISED_FILE)
            .exists()
    );
    let binned = store.reader().unwrap().binned(EPISODE).unwrap().unwrap();
    assert_eq!(binned.from, "command line");

    let output = episode(&folder, &["restore", EPISODE]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(data.join(EPISODE).join(FINALISED_FILE).exists());
    assert_eq!(store.reader().unwrap().binned(EPISODE).unwrap(), None);

    // One that isn't there: told why, with its cause code.
    let output = episode(&folder, &["delete", "20260310T130000.000Z-00000009"]);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.status.code(), Some(1), "{stderr}");
    assert!(stderr.contains("api.episode_not_found"), "{stderr}");
}
