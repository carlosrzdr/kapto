// SPDX-License-Identifier: Apache-2.0

//! Finding and reading `kapto.toml`.

use std::io;
use std::path::{Path, PathBuf};

use kapto_core::{ErrorCode, KaptoError};

/// The contents of a configuration file and where it was read from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConfigFile {
    /// The file's path, used in messages and as the source of its values.
    pub path: PathBuf,
    /// The file's text.
    pub contents: String,
}

/// Where `kapto.toml` is looked for when no path is given: the
/// `KAPTO_CONFIG` environment variable, then `$XDG_CONFIG_HOME/kapto/`, then
/// `~/.config/kapto/`. `env` looks up environment variables.
pub fn default_config_path(env: &dyn Fn(&str) -> Option<String>) -> PathBuf {
    let set = |name: &str| env(name).filter(|value| !value.is_empty());
    if let Some(path) = set("KAPTO_CONFIG") {
        PathBuf::from(path)
    } else if let Some(config_home) = set("XDG_CONFIG_HOME") {
        Path::new(&config_home).join("kapto/kapto.toml")
    } else if let Some(home) = set("HOME") {
        Path::new(&home).join(".config/kapto/kapto.toml")
    } else {
        PathBuf::from("kapto.toml")
    }
}

/// Reads the configuration file at `path`.
///
/// A missing file is fine when the path is only the default location
/// (`explicit` is false): Kapto then runs on defaults. A file the user named
/// explicitly must exist.
///
/// # Errors
///
/// `config.file_not_found` if an explicitly named file doesn't exist;
/// `config.unreadable` if the file exists but can't be read.
pub fn read_config_file(path: &Path, explicit: bool) -> Result<Option<ConfigFile>, KaptoError> {
    match std::fs::read_to_string(path) {
        Ok(contents) => Ok(Some(ConfigFile {
            path: path.to_owned(),
            contents,
        })),
        Err(error) if error.kind() == io::ErrorKind::NotFound && !explicit => Ok(None),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            Err(KaptoError::new(ErrorCode::ConfigFileNotFound).detail(path.display().to_string()))
        }
        Err(error) => Err(KaptoError::new(ErrorCode::ConfigUnreadable)
            .detail(path.display().to_string())
            .source(error)),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;

    fn env(vars: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let vars: HashMap<String, String> = vars
            .iter()
            .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
            .collect();
        move |name| vars.get(name).cloned()
    }

    #[test]
    fn kapto_config_variable_wins_over_the_standard_locations() {
        let env = env(&[
            ("KAPTO_CONFIG", "/config/kapto.toml"),
            ("XDG_CONFIG_HOME", "/home/u/.cfg"),
            ("HOME", "/home/u"),
        ]);
        assert_eq!(default_config_path(&env), Path::new("/config/kapto.toml"));
    }

    #[test]
    fn xdg_config_home_is_used_before_the_home_directory() {
        let both = env(&[("XDG_CONFIG_HOME", "/home/u/.cfg"), ("HOME", "/home/u")]);
        assert_eq!(
            default_config_path(&both),
            Path::new("/home/u/.cfg/kapto/kapto.toml")
        );
        let home_only = env(&[("HOME", "/home/u")]);
        assert_eq!(
            default_config_path(&home_only),
            Path::new("/home/u/.config/kapto/kapto.toml")
        );
    }

    #[test]
    fn empty_variables_count_as_unset() {
        let env = env(&[("KAPTO_CONFIG", ""), ("HOME", "/home/u")]);
        assert_eq!(
            default_config_path(&env),
            Path::new("/home/u/.config/kapto/kapto.toml")
        );
    }

    #[test]
    fn a_missing_default_file_means_running_on_defaults() {
        let path = Path::new("/nonexistent/kapto/kapto.toml");
        assert_eq!(read_config_file(path, false).unwrap(), None);
    }

    #[test]
    fn a_missing_explicit_file_is_an_error() {
        let path = Path::new("/nonexistent/kapto/kapto.toml");
        let error = read_config_file(path, true).unwrap_err();
        assert_eq!(error.code(), ErrorCode::ConfigFileNotFound);
    }

    #[test]
    fn a_path_that_cannot_be_read_as_a_file_is_reported_as_unreadable() {
        // A directory exists but can't be read as text.
        let error = read_config_file(&std::env::temp_dir(), true).unwrap_err();
        assert_eq!(error.code(), ErrorCode::ConfigUnreadable);
    }

    #[test]
    fn an_existing_file_is_read_with_its_path() {
        let path =
            std::env::temp_dir().join(format!("kapto-config-test-{}.toml", std::process::id()));
        std::fs::write(&path, "[server]\nport = 7701\n").unwrap();
        let file = read_config_file(&path, true).unwrap().unwrap();
        std::fs::remove_file(&path).unwrap();
        assert_eq!(file.path, path);
        assert_eq!(file.contents, "[server]\nport = 7701\n");
    }
}
