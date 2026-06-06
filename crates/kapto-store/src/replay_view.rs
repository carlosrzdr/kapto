// SPDX-License-Identifier: Apache-2.0

//! What identifies an episode's replay view, the file the web UI's player
//! shows a recorded episode from: kept here, beside the other formats Kapto
//! writes, so the server can tell a view of another version, to be made
//! again, without the code that writes it. The format is specified in
//! `docs/reference/replay-view.md`.

use std::fs::File;
use std::io::Read;
use std::path::Path;

/// What a view starts and ends with.
pub const VIEW_MAGIC: &[u8; 4] = b"KVRV";
/// The view's version; a view of another is made again.
pub const VIEW_VERSION: u16 = 1;

/// The version of the view at `path`, or `None` if there is none there or
/// it isn't a view.
#[must_use]
pub fn view_version(path: &Path) -> Option<u16> {
    let mut start = [0_u8; 6];
    File::open(path).ok()?.read_exact(&mut start).ok()?;
    let [m0, m1, m2, m3, v0, v1] = start;
    (&[m0, m1, m2, m3] == VIEW_MAGIC).then_some(u16::from_le_bytes([v0, v1]))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_views_version_is_read_from_its_start_and_anything_else_has_none() {
        let folder =
            std::env::temp_dir().join(format!("kapto-{}-view-version", std::process::id()));
        std::fs::create_dir_all(&folder).unwrap();
        let file = |name: &str, bytes: &[u8]| {
            let path = folder.join(name);
            std::fs::write(&path, bytes).unwrap();
            path
        };
        assert_eq!(
            view_version(&file("a.view", b"KVRV\x02\x00\x00\x00rest")),
            Some(2)
        );
        assert_eq!(view_version(&file("b.view", b"KVR")), None);
        assert_eq!(view_version(&file("c.view", b"RRD\x00\x01\x00")), None);
        assert_eq!(view_version(&folder.join("missing.view")), None);
        std::fs::remove_dir_all(&folder).unwrap();
    }
}
