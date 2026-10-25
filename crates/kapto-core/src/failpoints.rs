// SPDX-License-Identifier: Apache-2.0

//! Failpoints: named places tests can stop a process at, to see what it
//! does around them, for example closing an episode, held while capture
//! carries on, or ended there as a crash would. They exist only with the
//! `failpoints` feature, which release builds never have; without it
//! [`failpoint!`](crate::failpoint) is nothing at all.
//!
//! A process built with it reads `KAPTO_FAILPOINTS` once: failpoints
//! separated by `;`, each `name=hold:<path>`, held while the file at
//! `<path>` exists, or `name=abort`, ending the process at once.

/// The environment variable failpoints are set by.
pub const FAILPOINTS_VAR: &str = "KAPTO_FAILPOINTS";

#[cfg(feature = "failpoints")]
mod armed {
    use std::path::PathBuf;
    use std::sync::OnceLock;
    use std::time::Duration;

    /// What a failpoint does when reached.
    #[derive(Debug, PartialEq, Eq)]
    enum Action {
        Hold(PathBuf),
        Abort,
    }

    fn parse(text: &str) -> Vec<(String, Action)> {
        text.split(';')
            .filter_map(|point| {
                let (name, action) = point.trim().split_once('=')?;
                let action = match action.split_once(':') {
                    Some(("hold", path)) => Action::Hold(PathBuf::from(path)),
                    None if action == "abort" => Action::Abort,
                    _ => return None,
                };
                Some((name.to_owned(), action))
            })
            .collect()
    }

    /// Does what failpoint `name` was set to, if anything.
    pub fn reached(name: &str) {
        static SET: OnceLock<Vec<(String, Action)>> = OnceLock::new();
        let set =
            SET.get_or_init(|| parse(&std::env::var(super::FAILPOINTS_VAR).unwrap_or_default()));
        for (point, action) in set {
            if point != name {
                continue;
            }
            match action {
                Action::Hold(path) => {
                    while path.exists() {
                        std::thread::sleep(Duration::from_millis(10));
                    }
                }
                Action::Abort => std::process::abort(),
            }
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn failpoints_are_read_as_holds_and_aborts_and_the_rest_left_out() {
            assert_eq!(
                parse("closing.manifest=hold:/tmp/held; closing.rename=abort;bad;x=sleep:1"),
                [
                    (
                        "closing.manifest".to_owned(),
                        Action::Hold(PathBuf::from("/tmp/held"))
                    ),
                    ("closing.rename".to_owned(), Action::Abort),
                ]
            );
        }
    }
}

#[cfg(feature = "failpoints")]
pub use armed::reached;

/// Does what failpoint `name` is set to by [`FAILPOINTS_VAR`]: holds or
/// aborts the process.
#[cfg(feature = "failpoints")]
#[macro_export]
macro_rules! failpoint {
    ($name:literal) => {
        $crate::failpoints::reached($name)
    };
}

/// Failpoint `name`; without the `failpoints` feature, nothing.
#[cfg(not(feature = "failpoints"))]
#[macro_export]
macro_rules! failpoint {
    ($name:literal) => {};
}
