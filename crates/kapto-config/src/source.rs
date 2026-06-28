// SPDX-License-Identifier: Apache-2.0

//! Where a configuration value came from.

use std::fmt;
use std::path::PathBuf;

/// The layer that provided a value. Later layers override earlier ones:
/// built-in default, then configuration file, then environment variable, then
/// command-line flag.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Source {
    /// Kapto's built-in default.
    Default,
    /// A line in a configuration file.
    File {
        /// The file.
        path: PathBuf,
        /// The 1-based line the value is on.
        line: usize,
    },
    /// An environment variable.
    Env {
        /// The variable's name, e.g. `KAPTO_SERVER_PORT`.
        var: String,
    },
    /// A command-line flag.
    Cli {
        /// The flag as typed, e.g. `--port`.
        flag: String,
    },
}

impl fmt::Display for Source {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Default => f.write_str("default"),
            Self::File { path, line } => write!(f, "{}:{line}", path.display()),
            Self::Env { var } => write!(f, "environment variable {var}"),
            Self::Cli { flag } => write!(f, "flag {flag}"),
        }
    }
}

/// A configuration value together with where it came from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Effective<T> {
    /// The value in use.
    pub value: T,
    /// The layer that provided it.
    pub source: Source,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sources_describe_themselves_for_people() {
        assert_eq!(Source::Default.to_string(), "default");
        let file = Source::File {
            path: PathBuf::from("/config/kapto.toml"),
            line: 3,
        };
        assert_eq!(file.to_string(), "/config/kapto.toml:3");
        let env = Source::Env {
            var: "KAPTO_SERVER_PORT".into(),
        };
        assert_eq!(env.to_string(), "environment variable KAPTO_SERVER_PORT");
        let cli = Source::Cli {
            flag: "--server-port".into(),
        };
        assert_eq!(cli.to_string(), "flag --server-port");
    }
}
