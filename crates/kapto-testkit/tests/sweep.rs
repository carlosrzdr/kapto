// SPDX-License-Identifier: Apache-2.0

//! Old scratch folders of tests that have ended are removed; the rest stay.

#![allow(
    clippy::unwrap_used,
    reason = "test helpers fail the test loudly when the setup itself breaks"
)]

use std::fs::File;
use std::path::Path;
use std::time::{Duration, SystemTime};

use kapto_testkit::{KEPT_FOR, TempDir, sweep};

/// A scratch folder named as `scratch` names them, last changed `age` ago.
fn folder(temp: &Path, name: &str, age: Duration) {
    let path = temp.join(name);
    std::fs::create_dir_all(&path).unwrap();
    File::open(&path)
        .unwrap()
        .set_modified(SystemTime::now() - age)
        .unwrap();
}

#[cfg(target_os = "linux")]
#[test]
fn folders_of_tests_that_ended_long_ago_are_removed_and_the_rest_kept() {
    let temp = TempDir::new("sweep");
    // No process ever has this ID: the kernel's limit is far below it.
    let ended = u32::MAX;
    let running = std::process::id();
    let long_ago = KEPT_FOR + Duration::from_secs(60);
    folder(&temp.0, &format!("kt-{ended}-old"), long_ago);
    folder(
        &temp.0,
        &format!("kt-{ended}-recent"),
        Duration::from_secs(60),
    );
    folder(&temp.0, &format!("kt-{running}-running"), long_ago);
    folder(&temp.0, "someone-elses", long_ago);

    sweep(&temp.0, KEPT_FOR);

    let mut left: Vec<String> = std::fs::read_dir(&temp.0)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .collect();
    left.sort();
    let mut kept = [
        format!("kt-{ended}-recent"),
        format!("kt-{running}-running"),
        "someone-elses".to_owned(),
    ];
    kept.sort();
    assert_eq!(left, kept);
}
