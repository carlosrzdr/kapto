// SPDX-License-Identifier: Apache-2.0

//! A destination's credentials, read from where its `credentials` says
//! when an upload starts, never from the configuration itself.
//!
//! Each kind of storage needs a few values, named the same wherever they
//! come from: `access_key_id`, `secret_access_key` and `session_token` for
//! S3, `service_account_key` (the key's JSON) for Google Cloud Storage,
//! `account` and `access_key` for Azure. `{ env = "SITE_NAS" }` reads
//! them from environment variables named after it, upper case
//! (`SITE_NAS_ACCESS_KEY_ID`); `{ file = "/run/secrets/site_nas" }` from a
//! TOML file holding them under their names, as a mounted secret does.
//! They are read again for every upload, so a rotated secret is used from
//! the next one on.
//!
//! A value read is a [`Secret`]: it never shows itself in a message, a
//! log or a debug print, only to the store it is handed to.

use std::fmt;
use std::path::PathBuf;

use kapto_config::Credentials;
use kapto_core::{ErrorCode, KaptoError};

/// A credential's value, shown to nothing but the store.
#[derive(Clone, PartialEq, Eq)]
pub struct Secret(String);

impl Secret {
    /// The value, to hand to the store.
    #[must_use]
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Secret([redacted])")
    }
}

impl fmt::Display for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("[redacted]")
    }
}

/// Where a destination's credentials are read from.
pub trait CredentialSource {
    /// The value called `name` (e.g. `secret_access_key`), if the source
    /// has it.
    ///
    /// # Errors
    ///
    /// `upload.credentials_missing` if the source itself can't be read: a
    /// file that isn't there or isn't TOML. The message never holds a value.
    fn get(&self, name: &str) -> Result<Option<Secret>, KaptoError>;

    /// Where the values come from, for messages, e.g. `the environment
    /// variables SITE_NAS_*`.
    fn describe(&self) -> String;
}

/// Environment variables named after a prefix: `SITE_NAS_ACCESS_KEY_ID`.
pub struct EnvSource<'a> {
    prefix: String,
    env: &'a dyn Fn(&str) -> Option<String>,
}

// Only the prefix: the environment it reads holds credentials.
impl fmt::Debug for EnvSource<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("EnvSource")
            .field("prefix", &self.prefix)
            .finish_non_exhaustive()
    }
}

impl<'a> EnvSource<'a> {
    /// Variables starting with `prefix`, read through `env`.
    #[must_use]
    pub fn new(prefix: &str, env: &'a dyn Fn(&str) -> Option<String>) -> Self {
        Self {
            prefix: prefix.to_owned(),
            env,
        }
    }

    fn variable(&self, name: &str) -> String {
        format!("{}_{}", self.prefix, name.to_ascii_uppercase())
    }
}

impl CredentialSource for EnvSource<'_> {
    fn get(&self, name: &str) -> Result<Option<Secret>, KaptoError> {
        Ok((self.env)(&self.variable(name))
            .filter(|value| !value.is_empty())
            .map(Secret))
    }

    fn describe(&self) -> String {
        format!("the environment variables {}_*", self.prefix)
    }
}

/// A TOML file holding the values under their names.
#[derive(Debug)]
pub struct FileSource {
    path: PathBuf,
}

impl FileSource {
    /// The file at `path`, read when a value is asked for.
    #[must_use]
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }
}

impl CredentialSource for FileSource {
    fn get(&self, name: &str) -> Result<Option<Secret>, KaptoError> {
        let missing = |detail: String| {
            KaptoError::new(ErrorCode::UploadCredentialsMissing)
                .detail(detail)
                .context("file", self.path.display().to_string())
        };
        let text = std::fs::read_to_string(&self.path)
            .map_err(|error| missing(format!("the credentials file can't be read: {error}")))?;
        // The parser's own message may quote the file, secrets and all.
        let table = text.parse::<toml_edit::DocumentMut>().map_err(|_| {
            missing("the credentials file isn't TOML: `name = \"value\"` lines".to_owned())
        })?;
        let value = table.get(name).map(|item| item.as_str());
        match value {
            None => Ok(None),
            Some(Some(value)) => {
                Ok(Some(Secret(value.to_owned())).filter(|value| !value.0.is_empty()))
            }
            Some(None) => Err(missing(format!(
                "`{name}` in the credentials file isn't text"
            ))),
        }
    }

    fn describe(&self) -> String {
        format!("the file {}", self.path.display())
    }
}

/// The source `credentials` names, reading the environment through `env`.
#[must_use]
pub fn source<'a>(
    credentials: &Credentials,
    env: &'a dyn Fn(&str) -> Option<String>,
) -> Box<dyn CredentialSource + 'a> {
    match credentials {
        Credentials::Env(prefix) => Box::new(EnvSource::new(prefix, env)),
        Credentials::File(path) => Box::new(FileSource::new(path.clone())),
    }
}

/// The value `name` from `source`, which must have it.
///
/// # Errors
///
/// `upload.credentials_missing`, naming the value and where it was looked
/// for.
pub fn required(source: &dyn CredentialSource, name: &str) -> Result<Secret, KaptoError> {
    source.get(name)?.ok_or_else(|| {
        KaptoError::new(ErrorCode::UploadCredentialsMissing)
            .detail(format!("`{name}` isn't in {}", source.describe()))
    })
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// What every source does, whatever it reads: a value it has comes
    /// back, one it hasn't is absent, and neither shows in a message.
    pub(crate) fn conformance(source: &dyn CredentialSource, secret: &str) {
        let found = source.get("secret_access_key").unwrap().unwrap();
        assert_eq!(found.expose(), secret);
        assert!(!format!("{found:?} {found}").contains(secret));
        assert_eq!(source.get("session_token").unwrap(), None);
        let error = required(source, "session_token").unwrap_err();
        assert_eq!(error.code(), ErrorCode::UploadCredentialsMissing);
        assert!(error.to_string().contains("session_token"), "{error}");
        assert!(!error.to_string().contains(secret), "{error}");
    }

    #[test]
    fn environment_variables_are_named_after_the_prefix_in_upper_case() {
        let env =
            |name: &str| (name == "SITE_NAS_SECRET_ACCESS_KEY").then(|| "kapto-s3cr3t".to_owned());
        let source = EnvSource::new("SITE_NAS", &env);
        conformance(&source, "kapto-s3cr3t");
        assert_eq!(source.describe(), "the environment variables SITE_NAS_*");
    }

    #[test]
    fn an_environment_source_shows_its_prefix_but_no_value() {
        let env = |_: &str| Some("kapto-s3cr3t".to_owned());
        let shown = format!("{:?}", EnvSource::new("SITE_NAS", &env));
        assert_eq!(shown, "EnvSource { prefix: \"SITE_NAS\", .. }");
    }

    #[test]
    fn a_credentials_file_holds_the_values_under_their_names() {
        let folder = kapto_testkit::TempDir::new("credentials-file");
        let path = folder.write(
            "site_nas",
            b"access_key_id = \"kapto\"\nsecret_access_key = \"kapto-s3cr3t\"\n",
        );
        conformance(&FileSource::new(path), "kapto-s3cr3t");
    }

    #[test]
    fn a_file_missing_or_not_toml_is_refused_without_quoting_it() {
        let folder = kapto_testkit::TempDir::new("credentials-broken");
        let broken = folder.write("site_nas", b"secret_access_key = kapto-s3cr3t\n");
        for path in [broken, folder.0.join("missing")] {
            let error = FileSource::new(path).get("secret_access_key").unwrap_err();
            assert_eq!(error.code(), ErrorCode::UploadCredentialsMissing);
            assert!(!error.to_string().contains("kapto-s3cr3t"), "{error}");
        }
    }
}
