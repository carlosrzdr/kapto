// SPDX-License-Identifier: Apache-2.0

//! Embeds the built web UI (`ui/dist`, made by `cargo xtask ui build` or the
//! image build) in the binary. Without it, a page saying how to build it is
//! embedded instead, so `cargo build` never needs Node.js.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

fn main() {
    let manifest = PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").unwrap_or_default());
    let ui = manifest.join("../../ui");
    let dist = ui.join("dist");
    let mut files = Vec::new();
    if dist.join("index.html").is_file() {
        collect(&dist, &dist, &mut files);
        println!("cargo::rerun-if-changed={}", dist.display());
    } else {
        // Building the UI adds `dist` to this folder, which reruns this.
        println!("cargo::rerun-if-changed={}", ui.display());
    }
    files.sort();

    let mut code = String::from("/// The UI's files, by the path they are served at.\n");
    code.push_str("pub(crate) const FILES: &[(&str, &[u8])] = &[\n");
    for (path, file) in &files {
        let _ = writeln!(
            code,
            "    ({path:?}, include_bytes!({:?})),",
            file.display()
        );
    }
    code.push_str("];\n");
    let _ = writeln!(
        code,
        "/// Whether the UI was built into this binary.\npub(crate) const BUILT: bool = {};",
        !files.is_empty()
    );
    let out = PathBuf::from(std::env::var_os("OUT_DIR").unwrap_or_default());
    if let Err(error) = std::fs::write(out.join("ui_files.rs"), code) {
        println!("cargo::error=can't write the UI's file list: {error}");
    }
}

/// Every file under `folder`, as (path served at, absolute path).
fn collect(root: &Path, folder: &Path, files: &mut Vec<(String, PathBuf)>) {
    let Ok(entries) = std::fs::read_dir(folder) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect(root, &path, files);
        } else if let (Ok(relative), Ok(absolute)) = (path.strip_prefix(root), path.canonicalize())
        {
            let served = format!("/{}", relative.to_string_lossy().replace('\\', "/"));
            files.push((served, absolute));
        }
    }
}
