// SPDX-License-Identifier: Apache-2.0

//! `kapto upload check`: each destination tested as an upload would use
//! it, run as a real process.

#![allow(
    clippy::unwrap_used,
    reason = "test helpers fail the test loudly when the setup itself breaks"
)]

use std::path::Path;
use std::process::{Command, Output};

use kapto_testkit::scratch;

/// `kapto upload check` with `args`, the configuration `toml`, and only
/// the environment variables `env`.
fn check(folder: &Path, toml: &str, args: &[&str], env: &[(&str, String)]) -> Output {
    let config = folder.join("kapto.toml");
    std::fs::write(&config, toml).unwrap();
    Command::new(env!("CARGO_BIN_EXE_kapto"))
        .arg("--config")
        .arg(&config)
        .args(["upload", "check"])
        .args(args)
        .env_clear()
        .envs(env.iter().map(|(name, value)| (*name, value)))
        .output()
        .unwrap()
}

#[test]
fn every_destination_is_probed_and_one_out_of_reach_fails_with_its_cause() {
    let folder = scratch("upload-check");
    let bucket = folder.join("bucket");
    let toml = format!(
        "[[upload.destination]]\nname = \"site\"\nurl = \"file://{}\"\nprefix = \"fleet-a\"\n\
         [[upload.destination]]\nname = \"away\"\nurl = \"s3://robot-data\"\n\
         endpoint = \"http://127.0.0.1:9\"\ncredentials = {{ env = \"AWAY\" }}\n",
        bucket.display()
    );
    let credentials = [
        ("AWAY_ACCESS_KEY_ID", "kapto".to_owned()),
        ("AWAY_SECRET_ACCESS_KEY", "kapto-s3cr3t".to_owned()),
    ];
    let output = check(&folder, &toml, &[], &credentials);
    let (stdout, stderr) = (
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    assert_eq!(output.status.code(), Some(1), "{stdout}{stderr}");
    assert!(
        stdout.contains("site: ok, a probe written, read back and removed"),
        "{stdout}"
    );
    assert!(
        stderr.contains("away: error[upload.unreachable]"),
        "{stderr}"
    );
    assert!(!format!("{stdout}{stderr}").contains("kapto-s3cr3t"));
    // Nothing is left behind where the probe could be removed.
    assert_eq!(
        std::fs::read_dir(bucket.join("fleet-a")).unwrap().count(),
        0
    );

    // Only the one named, which is fine on its own.
    let output = check(&folder, &toml, &["site"], &credentials);
    assert!(output.status.success());
}

#[test]
fn a_destination_not_configured_is_named_as_missing() {
    let folder = scratch("upload-check-missing");
    for (toml, args, said) in [
        ("", &[][..], "has no `[[upload.destination]]`"),
        (
            "[[upload.destination]]\nname = \"site\"\nurl = \"file:///tmp/kapto-never-made\"\n",
            &["elsewhere"][..],
            "no `[[upload.destination]]` is named `elsewhere`",
        ),
    ] {
        let output = check(&folder, toml, args, &[]);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert_eq!(output.status.code(), Some(1), "{stderr}");
        assert!(stderr.contains("error[upload.no_destination]"), "{stderr}");
        assert!(stderr.contains(said), "{stderr}");
    }
}

#[test]
#[ignore = "needs the RustFS server `cargo xtask storage` starts"]
fn s3_compatible_storage_passes_the_check_with_credentials_by_reference() {
    let folder = scratch("upload-check-s3");
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(kapto_upload::rustfs::bucket("check"));
    let toml = format!(
        "[[upload.destination]]\nname = \"site\"\nurl = \"s3://check\"\nprefix = \"fleet-a\"\n\
         endpoint = \"{}\"\ncredentials = {{ env = \"RUSTFS\" }}\n",
        kapto_upload::rustfs::endpoint()
    );
    let env = [
        ("RUSTFS_ACCESS_KEY_ID", "AWS_ACCESS_KEY_ID"),
        ("RUSTFS_SECRET_ACCESS_KEY", "AWS_SECRET_ACCESS_KEY"),
        ("AWS_REGION", "AWS_REGION"),
    ]
    .map(|(name, from)| (name, std::env::var(from).unwrap()));
    let output = check(&folder, &toml, &[], &env);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "{stdout}{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(stdout.contains("site: ok"), "{stdout}");
}
