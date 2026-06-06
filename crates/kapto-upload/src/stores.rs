// SPDX-License-Identifier: Apache-2.0

//! The store a destination's `url` names, set up from its settings: S3 and
//! S3-compatible storage (`s3://bucket`, with `endpoint` for one other than
//! AWS), Google Cloud Storage (`gs://bucket`), Azure Blob Storage
//! (`az://container`) or a folder (`file:///folder`).
//!
//! Each store is built from the provider's own settings in Kapto's
//! environment first (`AWS_*`, `GOOGLE_*`, `AZURE_*`, as each provider's
//! tools read them), then the destination's `endpoint` and `region` and
//! the credentials its `credentials` names over them
//! ([`crate::credentials`]), so a machine set up for a provider needs
//! nothing more. Connecting
//! and each request time out as `[upload]` says. A store never tries a
//! request again by itself: the engine does, as `upload.max_attempts` and
//! its waits say, which the store's own tries would multiply.

use std::num::NonZeroU64;
use std::sync::Arc;
use std::time::Duration;

use kapto_config::{UploadDestination, UploadSettings, Verification};
use kapto_core::{ErrorCode, KaptoError};
use object_store::aws::{AmazonS3Builder, Checksum};
use object_store::azure::MicrosoftAzureBuilder;
use object_store::gcp::GoogleCloudStorageBuilder;
use object_store::local::LocalFileSystem;
use object_store::multipart::MultipartStore;
use object_store::{ClientOptions, ObjectStore, RetryConfig};

use crate::credentials::{self, required};
use crate::episode::{Policy, Store, Verify};
use crate::pace::part_bytes;

/// A destination's store, with what the engine may rely on it for.
pub struct Opened {
    objects: Arc<dyn ObjectStore>,
    multipart: Option<Arc<dyn MultipartStore>>,
    /// Whether the store checks each file's SHA-256 as it takes it,
    /// refusing one that arrived different.
    pub checks_sha256: bool,
    /// The smallest part, but the last, the store takes in a file put in
    /// parts.
    pub smallest_part_bytes: u64,
    /// The most parts the store takes in one file.
    pub most_parts: u64,
}

impl std::fmt::Debug for Opened {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // The store's own debug print may hold its credentials; its
        // display names only the bucket.
        f.debug_struct("Opened")
            .field("objects", &self.objects.to_string())
            .field("in_parts", &self.multipart.is_some())
            .field("checks_sha256", &self.checks_sha256)
            .field("smallest_part_bytes", &self.smallest_part_bytes)
            .field("most_parts", &self.most_parts)
            .finish()
    }
}

impl Opened {
    /// The store as the engine takes it.
    #[must_use]
    pub fn store(&self) -> Store<'_> {
        Store {
            objects: self.objects.as_ref(),
            multipart: self.multipart.as_deref(),
        }
    }
}

/// How an upload goes, from a destination's settings and what its store
/// does: parts about one second at the bandwidth cap, if there is one,
/// within what the store takes. The worker's and the upload benchmark's.
#[must_use]
pub fn policy(settings: &UploadSettings, opened: &Opened) -> Policy {
    let bandwidth = NonZeroU64::new(u64::from(settings.bandwidth_mib.value) << 20);
    let configured = settings.part_size_mib.value.saturating_mul(1 << 20);
    let part = part_bytes(configured, bandwidth, opened.smallest_part_bytes);
    Policy {
        part_bytes: usize::try_from(part).unwrap_or(usize::MAX),
        most_parts: opened.most_parts,
        // Each part in flight is a request in flight.
        parallel_parts: usize::try_from(
            settings
                .parallel_parts
                .value
                .min(settings.max_in_flight.value),
        )
        .unwrap_or(usize::MAX),
        max_in_flight: usize::try_from(settings.max_in_flight.value).unwrap_or(usize::MAX),
        max_attempts: settings.max_attempts.value,
        verify: match settings.verify.value {
            Verification::ServerChecksum => Verify::ServerChecksum,
            Verification::ReadBack => Verify::ReadBack,
            Verification::SizeOnly => Verify::SizeOnly,
        },
        store_checks_sha256: opened.checks_sha256,
        marker: settings.marker.value,
        bandwidth,
    }
}

/// The store `destination` names, its credentials read through `env`
/// where they are environment variables.
///
/// # Errors
///
/// `upload.destination_unusable` if its settings don't make a store: a
/// `url` the provider doesn't take, a setting its kind has no use for, or
/// one the provider needs and Kapto's environment doesn't have either.
/// `upload.credentials_missing` if its credentials aren't where they are
/// said to be.
pub fn open(
    destination: &UploadDestination,
    env: &dyn Fn(&str) -> Option<String>,
) -> Result<Opened, KaptoError> {
    let unusable = |detail: String| {
        KaptoError::new(ErrorCode::UploadDestinationUnusable)
            .detail(detail)
            .context("destination", destination.name.clone())
    };
    let credentials = destination
        .credentials
        .as_ref()
        .map(|credentials| credentials::source(credentials, env));
    let credentials = credentials.as_deref();
    let named = |error: KaptoError| error.context("destination", destination.name.clone());
    let url = destination.url.as_str();
    let scheme = url.split_once("://").map_or(url, |(scheme, _)| scheme);
    if destination.region.is_some() && scheme != "s3" {
        return Err(unusable(format!(
            "`region` is for S3 storage; `{url}` has none"
        )));
    }
    let endpoint = destination.endpoint.as_deref();
    let plain_http = endpoint.is_some_and(|endpoint| endpoint.starts_with("http://"));
    let settings = &destination.settings;
    let client = ClientOptions::new()
        .with_connect_timeout(settings.timeout_connect.value)
        .with_timeout(settings.timeout_request.value)
        .with_allow_http(plain_http);
    let built = |error: object_store::Error| unusable(error.to_string());
    match scheme {
        "file" => {
            if endpoint.is_some() || credentials.is_some() {
                return Err(unusable(format!(
                    "`endpoint` and `credentials` are for cloud storage; `{url}` is a folder"
                )));
            }
            folder(url.trim_start_matches("file://")).map_err(unusable)
        }
        "s3" => {
            let mut builder = AmazonS3Builder::from_env()
                .with_url(url)
                .with_checksum_algorithm(Checksum::SHA256)
                .with_client_options(client)
                .with_retry(no_retries());
            if let Some(endpoint) = endpoint {
                builder = builder.with_endpoint(endpoint);
            }
            if let Some(region) = &destination.region {
                builder = builder.with_region(region);
            }
            if let Some(source) = credentials {
                let key = |name| required(source, name).map_err(named);
                builder = builder
                    .with_access_key_id(key("access_key_id")?.expose())
                    .with_secret_access_key(key("secret_access_key")?.expose());
                if let Some(token) = source.get("session_token").map_err(named)? {
                    builder = builder.with_token(token.expose());
                }
            }
            let store = Arc::new(builder.build().map_err(built)?);
            Ok(cloud(store, true, (FIVE_MIB, 10_000)))
        }
        "gs" => {
            let mut builder = GoogleCloudStorageBuilder::from_env()
                .with_url(url)
                .with_client_options(client)
                .with_retry(no_retries());
            if let Some(endpoint) = endpoint {
                builder = builder.with_base_url(endpoint);
            }
            if let Some(source) = credentials {
                let key = required(source, "service_account_key").map_err(named)?;
                builder = builder.with_service_account_key(key.expose());
            }
            let store = Arc::new(builder.build().map_err(built)?);
            Ok(cloud(store, false, (FIVE_MIB, 10_000)))
        }
        "az" => {
            let mut builder = MicrosoftAzureBuilder::from_env()
                .with_url(url)
                .with_client_options(client)
                .with_retry(no_retries());
            if let Some(endpoint) = endpoint {
                builder = builder.with_endpoint(endpoint.to_owned());
            }
            if let Some(source) = credentials {
                if let Some(account) = source.get("account").map_err(named)? {
                    builder = builder.with_account(account.expose());
                }
                let key = required(source, "access_key").map_err(named)?;
                builder = builder.with_access_key(key.expose());
            }
            Ok(cloud(
                Arc::new(builder.build().map_err(built)?),
                false,
                (0, 50_000),
            ))
        }
        _ => Err(unusable(format!(
            "`{url}` isn't storage Kapto uploads to; a `url` starts with s3://, gs://, az:// or file://"
        ))),
    }
}

/// The smallest part, but the last, S3 and Google Cloud Storage take, in
/// at most 10 000 parts a file; Azure takes 50 000 blocks of any size.
const FIVE_MIB: u64 = 5 << 20;

/// A cloud store, which puts large files in parts, taking parts of at least
/// `smallest` bytes and at most `most` of them.
fn cloud<S: ObjectStore + MultipartStore>(
    store: Arc<S>,
    checks_sha256: bool,
    (smallest, most): (u64, u64),
) -> Opened {
    Opened {
        objects: store.clone(),
        multipart: Some(store),
        checks_sha256,
        smallest_part_bytes: smallest,
        most_parts: most,
    }
}

/// The folder at `path`, made if it isn't there. A folder doesn't check
/// what it is given, and puts a large file whole.
fn folder(path: &str) -> Result<Opened, String> {
    std::fs::create_dir_all(path)
        .map_err(|error| error.to_string())
        .and_then(|()| LocalFileSystem::new_with_prefix(path).map_err(|error| error.to_string()))
        .map(|store| Opened {
            objects: Arc::new(store),
            multipart: None,
            checks_sha256: false,
            smallest_part_bytes: 0,
            // Put whole, in one stream.
            most_parts: u64::MAX,
        })
        .map_err(|error| format!("the folder `{path}`: {error}"))
}

fn no_retries() -> RetryConfig {
    RetryConfig {
        max_retries: 0,
        retry_timeout: Duration::ZERO,
        ..RetryConfig::default()
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use kapto_config::{ConfigFile, DeploymentConfig, Inputs};

    use super::*;

    /// The destination `site_nas` at `url`, with the entry's other lines.
    fn destination(url: &str, lines: &str) -> UploadDestination {
        let file = ConfigFile {
            path: PathBuf::from("/config/kapto.toml"),
            contents: format!(
                "[[upload.destination]]\nname = \"site_nas\"\nurl = \"{url}\"\n{lines}"
            ),
        };
        let inputs = Inputs {
            file: Some(&file),
            env: &|_| None,
            cli: &[],
        };
        let mut config = DeploymentConfig::resolve(&inputs).unwrap();
        config.upload_destinations.remove(0)
    }

    #[test]
    fn s3_storage_elsewhere_than_aws_is_put_in_parts_and_checks_each_files_sha256() {
        let nas = destination(
            "s3://robot-data",
            "endpoint = \"http://nas.local:9000\"\nregion = \"eu-west-1\"\n",
        );
        let opened = open(&nas, &|_| None).unwrap();
        assert!(opened.checks_sha256);
        assert_eq!(
            (opened.smallest_part_bytes, opened.most_parts),
            (5 << 20, 10_000)
        );
        assert!(opened.store().multipart.is_some());
        assert!(
            opened.objects.to_string().contains("robot-data"),
            "{}",
            opened.objects
        );
    }

    #[test]
    fn a_folder_is_made_if_missing_and_puts_files_whole_unchecked() {
        let scratch = kapto_testkit::TempDir::new("a_folder_is_made_if_missing");
        let path = scratch.0.join("bucket");
        let opened = open(
            &destination(&format!("file://{}", path.display()), ""),
            &|_| None,
        )
        .unwrap();
        assert!(path.is_dir());
        assert!(!opened.checks_sha256);
        assert!(opened.store().multipart.is_none());
        assert_eq!(opened.smallest_part_bytes, 0);
    }

    #[test]
    fn settings_a_kind_of_storage_has_no_use_for_are_refused_naming_them() {
        let mut unknown = destination("s3://robot-data", "");
        unknown.url = "ftp://robot-data".to_owned();
        for (destination, named) in [
            (
                destination(
                    "file:///tmp/kapto-never-made",
                    "endpoint = \"http://nas.local:9000\"\n",
                ),
                "`endpoint`",
            ),
            (
                destination("gs://robot-data", "region = \"eu-west-1\"\n"),
                "`region`",
            ),
            (
                destination(
                    "file:///tmp/kapto-never-made",
                    "credentials = { env = \"SITE_NAS\" }\n",
                ),
                "`credentials`",
            ),
            (unknown, "ftp://"),
        ] {
            let error = open(&destination, &|_| None).unwrap_err();
            assert_eq!(error.code(), ErrorCode::UploadDestinationUnusable);
            assert!(error.to_string().contains(named), "{error}");
            assert_eq!(
                error.context_entries(),
                [("destination", "site_nas".to_owned())]
            );
        }
    }

    #[test]
    fn credentials_by_reference_are_read_for_each_kind_of_storage_and_never_shown() {
        // Base64, as Azure's keys are.
        let secret = "a2FwdG8tczNjcjN0LXZhbHVl";
        let env = |name: &str| match name {
            "SITE_NAS_ACCESS_KEY_ID" | "SITE_NAS_ACCOUNT" => Some("kapto".to_owned()),
            "SITE_NAS_SECRET_ACCESS_KEY" | "SITE_NAS_ACCESS_KEY" => Some(secret.to_owned()),
            _ => None,
        };
        let referenced = "credentials = { env = \"SITE_NAS\" }\n";
        for url in ["s3://robot-data", "az://robot-data"] {
            let opened = open(&destination(url, referenced), &env).unwrap();
            assert!(!format!("{opened:?}").contains(secret), "{opened:?}");
        }
        // Google's key is a whole JSON document, checked when the store is
        // built: one missing is named, with where it was looked for.
        let error = open(&destination("gs://robot-data", referenced), &env).unwrap_err();
        assert_eq!(error.code(), ErrorCode::UploadCredentialsMissing);
        assert!(
            error
                .to_string()
                .contains("`service_account_key` isn't in the environment variables SITE_NAS_*"),
            "{error}"
        );
        assert_eq!(
            error.context_entries(),
            [("destination", "site_nas".to_owned())]
        );
        let without_secret =
            |name: &str| (name == "SITE_NAS_ACCESS_KEY_ID").then(|| "kapto".to_owned());
        let error = open(&destination("s3://robot-data", referenced), &without_secret).unwrap_err();
        assert!(error.to_string().contains("`secret_access_key`"), "{error}");
    }
}
