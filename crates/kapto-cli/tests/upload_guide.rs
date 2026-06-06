// SPDX-License-Identifier: Apache-2.0

//! The guide to trying uploads: its configuration and credentials, as
//! written but for the server's port, pass `kapto upload check` against
//! the RustFS server of `compose.yaml`, in the bucket it makes, so the
//! guide can't drift from the server it starts.

#![allow(
    clippy::unwrap_used,
    reason = "test helpers fail the test loudly when the setup itself breaks"
)]

use std::path::Path;
use std::process::Command;

use kapto_testkit::scratch;

/// Where the guide has RustFS answer.
const DOCUMENTED_ENDPOINT: &str = "http://localhost:9000";

/// The text of each block fenced as `language` in `markdown`, in order.
fn blocks<'a>(markdown: &'a str, language: &str) -> Vec<&'a str> {
    let open = format!("```{language}\n");
    markdown
        .split(open.as_str())
        .skip(1)
        .filter_map(|rest| rest.split("```").next())
        .collect()
}

fn guide() -> String {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    std::fs::read_to_string(root.join("docs/guides/try-uploads.md")).unwrap()
}

#[test]
fn the_guide_sets_the_credentials_its_configuration_reads_and_checks_with_them() {
    let guide = guide();
    let toml = blocks(&guide, "toml")[0];
    assert!(toml.contains("credentials = { env = \"LOCAL\" }"), "{toml}");
    assert!(toml.contains(DOCUMENTED_ENDPOINT), "{toml}");
    let shell = blocks(&guide, "sh").concat();
    for line in [
        "export LOCAL_ACCESS_KEY_ID=",
        "export LOCAL_SECRET_ACCESS_KEY=",
        "kapto --config kapto.toml upload check",
        "docker compose run --rm rustfs-bucket",
    ] {
        assert!(shell.contains(line), "the guide's commands lack `{line}`");
    }
}

#[test]
#[ignore = "needs the RustFS server `cargo xtask storage` starts"]
fn the_try_uploads_guide_reaches_the_s3_bucket_compose_makes() {
    let folder = scratch("upload-guide-s3");
    let guide = guide();
    let config = folder.join("kapto.toml");
    let toml =
        blocks(&guide, "toml")[0].replace(DOCUMENTED_ENDPOINT, &kapto_upload::rustfs::endpoint());
    std::fs::write(&config, toml).unwrap();
    let env: Vec<(&str, &str)> = blocks(&guide, "sh")
        .iter()
        .flat_map(|block| block.lines())
        .filter_map(|line| line.strip_prefix("export ")?.split_once('='))
        .collect();
    assert_eq!(env.len(), 2, "{env:?}");
    let output = Command::new(env!("CARGO_BIN_EXE_kapto"))
        .arg("--config")
        .arg(&config)
        .args(["upload", "check"])
        .env_clear()
        .envs(env)
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "{stdout}{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(stdout.contains("local: ok"), "{stdout}");
}
