// SPDX-License-Identifier: Apache-2.0

//! Where episodes are uploaded: the `[[upload.destination]]` entries of
//! `kapto.toml`, each a bucket, the folder inside it, how episodes are laid
//! out there, and where its credentials come from, never the credentials
//! themselves:
//!
//! ```toml
//! [[upload.destination]]
//! name = "site_nas"
//! url = "s3://robot-data"
//! prefix = "fleet-a/robot-7"
//! endpoint = "http://nas.local:9000"
//! credentials = { env = "SITE_NAS" }
//! backoff_max = 30
//! ```
//!
//! Any `[upload]` setting repeated in an entry applies to that destination
//! alone. Destinations are read from the file only: environment variables
//! and flags set the `[upload]` values every destination starts from.

use std::path::PathBuf;

use kapto_core::name::is_name;
use kapto_core::{ErrorCode, KaptoError};
use toml_edit::{Item, TableLike};

use crate::file::ConfigFile;
use crate::located::{at_line, line_of, unknown_key};
use crate::suggest::closest;
use crate::upload::UploadSettings;

/// The kinds of storage a destination's `url` can name.
pub const SCHEMES: [&str; 4] = ["s3", "gs", "az", "file"];

/// How episodes are laid out under the prefix when nothing says otherwise.
pub const DEFAULT_LAYOUT: &str = "{robot}/{date}/{episode}";

/// What a layout may hold, besides plain names: the robot package's ID, when
/// the episode was recorded (in UTC, zero-padded so keys sort in time order:
/// `{date}` is `2026-03-23`, the others its parts and the hour), the
/// episode's ID, and the start of its manifest's checksum.
pub const PLACEHOLDERS: [&str; 8] = [
    "{robot}",
    "{date}",
    "{year}",
    "{month}",
    "{day}",
    "{hour}",
    "{episode}",
    "{sha8}",
];

/// A destination's own keys, besides the `[upload]` settings it may repeat.
const KEYS: [&str; 7] = [
    "name",
    "url",
    "prefix",
    "layout",
    "endpoint",
    "region",
    "credentials",
];

/// Where a destination's credentials come from, never their values.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Credentials {
    /// `{ env = "SITE_NAS" }`: environment variables named after it, read
    /// when uploading, e.g. `SITE_NAS_ACCESS_KEY_ID`.
    Env(String),
    /// `{ file = "/run/secrets/site_nas" }`: a mounted credentials file.
    File(PathBuf),
}

/// One `[[upload.destination]]` entry, checked.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UploadDestination {
    /// Its name, as statuses and the API show it.
    pub name: String,
    /// The bucket or container, e.g. `s3://robot-data`.
    pub url: String,
    /// The folder inside the bucket episodes go under; empty for its root.
    pub prefix: String,
    /// How each episode's key is made under the prefix, e.g.
    /// `{robot}/{date}/{episode}`.
    pub layout: String,
    /// The server, for S3-compatible storage other than AWS.
    pub endpoint: Option<String>,
    /// The provider's region, where it has them.
    pub region: Option<String>,
    /// Where its credentials come from; `None` for the provider's own
    /// defaults, such as the machine's role.
    pub credentials: Option<Credentials>,
    /// The `[upload]` settings, with what this entry repeats of them.
    pub settings: UploadSettings,
    /// The line its entry starts on.
    pub line: usize,
}

/// A destination's own values, before its settings are worked out.
pub(crate) struct Entry<'a> {
    pub(crate) destination: UploadDestination,
    /// The `[upload]` settings it repeats.
    pub(crate) overrides: Vec<(&'a str, &'a Item, usize)>,
}

/// Every `[[upload.destination]]` entry of `file`, without its settings
/// yet: `base` stands in for them until they are worked out.
pub(crate) fn read<'a>(
    file: &ConfigFile,
    entries: &'a Item,
    base: &UploadSettings,
    settings: &[&str],
    errors: &mut Vec<KaptoError>,
) -> Vec<Entry<'a>> {
    let Some(array) = entries.as_array_of_tables() else {
        let error = KaptoError::new(ErrorCode::ConfigInvalidValue)
            .detail("`upload.destination` must be [[upload.destination]] entries".to_owned());
        errors.push(at_line(error, file, 1));
        return Vec::new();
    };
    let mut read = Vec::new();
    for table in array {
        let line = table.span().map_or(1, |span| {
            crate::located::line_and_column(&file.contents, span.start).0
        });
        let before = errors.len();
        let entry = entry(file, table, line, base, settings, errors);
        if errors.len() == before {
            let duplicate = read
                .iter()
                .any(|other: &Entry<'_>| other.destination.name == entry.destination.name);
            if duplicate {
                let error = KaptoError::new(ErrorCode::ConfigInvalidValue).detail(format!(
                    "two destinations are named `{}`; each needs its own name",
                    entry.destination.name
                ));
                errors.push(at_line(error, file, line));
            }
            read.push(entry);
        }
    }
    read
}

fn entry<'a>(
    file: &ConfigFile,
    table: &'a dyn TableLike,
    line: usize,
    base: &UploadSettings,
    settings: &[&str],
    errors: &mut Vec<KaptoError>,
) -> Entry<'a> {
    let text = |key: &str| table.get(key).and_then(Item::as_str).map(str::to_owned);
    let mut overrides = Vec::new();
    for (key, item) in table.iter() {
        if KEYS.contains(&key) {
            continue;
        }
        let at = line_of(file, table, key);
        if settings.contains(&key) {
            overrides.push((key, item, at));
        } else if looks_like_a_secret(key) {
            let error = KaptoError::new(ErrorCode::ConfigSecretInline).detail(format!(
                "`{key}` in a destination; name where it comes from in `credentials` instead"
            ));
            errors.push(at_line(error, file, at));
        } else {
            let names = KEYS.iter().copied().chain(settings.iter().copied());
            let error = unknown_key(
                ErrorCode::ConfigUnknownKey,
                &format!("upload.destination.{key}"),
                closest(key, names),
            );
            errors.push(at_line(error, file, at));
        }
    }

    let mut problem = |key: &str, detail: String| {
        let line = if table.contains_key(key) {
            line_of(file, table, key)
        } else {
            line
        };
        errors.push(at_line(
            KaptoError::new(ErrorCode::ConfigInvalidValue).detail(detail),
            file,
            line,
        ));
    };
    let name = text("name").unwrap_or_default();
    if !is_name(&name) {
        problem(
            "name",
            "a destination needs a `name` of lowercase letters, digits and underscores, starting with a letter, e.g. \"site_nas\"".to_owned(),
        );
    }
    let url = text("url").unwrap_or_default();
    if let Err(detail) = check_url(&url) {
        problem("url", detail);
    }
    let prefix = text("prefix").unwrap_or_default();
    if !prefix.is_empty() && !is_key_path(&prefix) {
        problem(
            "prefix",
            format!(
                "`prefix` is `{prefix}`; it is folder names separated by `/`, none empty, `.` or `..`, e.g. \"fleet-a/robot-7\""
            ),
        );
    }
    let layout = text("layout").unwrap_or_else(|| DEFAULT_LAYOUT.to_owned());
    if let Err(detail) = check_layout(&layout) {
        problem("layout", detail);
    }
    let endpoint = text("endpoint");
    if endpoint.as_deref().is_some_and(|endpoint| {
        !endpoint.starts_with("http://") && !endpoint.starts_with("https://")
    }) {
        problem(
            "endpoint",
            "`endpoint` is the server's address starting with http:// or https://".to_owned(),
        );
    }
    let credentials = match table.get("credentials") {
        None => None,
        Some(item) => match credentials(item) {
            Ok(credentials) => Some(credentials),
            Err(detail) => {
                problem("credentials", detail);
                None
            }
        },
    };
    Entry {
        destination: UploadDestination {
            name,
            url,
            prefix,
            layout,
            endpoint,
            region: text("region"),
            credentials,
            settings: base.clone(),
            line,
        },
        overrides,
    }
}

/// Key names that hold a credential's value rather than say where it is.
fn looks_like_a_secret(key: &str) -> bool {
    [
        "secret",
        "password",
        "token",
        "access_key",
        "key_id",
        "session",
    ]
    .iter()
    .any(|word| key.contains(word))
}

fn check_url(url: &str) -> Result<(), String> {
    let expected = || {
        format!(
            "`url` is `{url}`; it is the bucket or container, e.g. \"s3://robot-data\", with one of the schemes {}",
            SCHEMES.map(|scheme| format!("{scheme}://")).join(", ")
        )
    };
    let (scheme, rest) = url.split_once("://").ok_or_else(expected)?;
    if !SCHEMES.contains(&scheme) {
        return Err(expected());
    }
    if scheme == "file" {
        return if rest.starts_with('/') {
            Ok(())
        } else {
            Err(format!(
                "`url` is `{url}`; a folder is named from the root, e.g. \"file:///srv/kapto\""
            ))
        };
    }
    match rest.split_once('/') {
        _ if rest.is_empty() => Err(expected()),
        Some((_, folder)) if !folder.is_empty() => Err(format!(
            "`url` is `{url}`; give the bucket alone and the folder inside it as `prefix = \"{folder}\"`"
        )),
        _ => Ok(()),
    }
}

/// Names separated by `/`, none empty, `.` or `..`, nor with a control
/// character: what storage takes as a key.
fn is_key_path(text: &str) -> bool {
    text.split('/').all(|name| !matches!(name, "" | "." | ".."))
        && !text.chars().any(char::is_control)
}

fn check_layout(layout: &str) -> Result<(), String> {
    let mut plain = layout.to_owned();
    for placeholder in PLACEHOLDERS {
        plain = plain.replace(placeholder, "x");
    }
    if plain.contains(['{', '}']) {
        return Err(format!(
            "`layout` is `{layout}`; it may hold {} and plain names",
            PLACEHOLDERS.join(", ")
        ));
    }
    if !layout.contains("{episode}") {
        return Err(format!(
            "`layout` is `{layout}`; it needs `{{episode}}`, so each episode has a key of its own"
        ));
    }
    if is_key_path(&plain) {
        Ok(())
    } else {
        Err(format!(
            "`layout` is `{layout}`; it is names separated by `/`, none empty, `.` or `..`"
        ))
    }
}

fn credentials(item: &Item) -> Result<Credentials, String> {
    let expected = || {
        "`credentials` says where they come from: { env = \"SITE_NAS\" } (environment variables named after it) or { file = \"/run/secrets/site_nas\" }".to_owned()
    };
    let table = item.as_table_like().ok_or_else(expected)?;
    let one = |key| table.get(key).and_then(Item::as_str);
    match (one("env"), one("file"), table.len()) {
        (Some(env), None, 1) => {
            let valid = env.starts_with(|c: char| c.is_ascii_uppercase())
                && env
                    .chars()
                    .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_');
            if valid {
                Ok(Credentials::Env(env.to_owned()))
            } else {
                // Not repeated: it may be a secret pasted in by mistake.
                Err("`credentials.env` is the start of environment variables' names, in capitals, e.g. \"SITE_NAS\", not a value".to_owned())
            }
        }
        (None, Some(file), 1) if file.starts_with('/') => {
            Ok(Credentials::File(PathBuf::from(file)))
        }
        _ => Err(expected()),
    }
}
