// SPDX-License-Identifier: Apache-2.0

//! Testing a destination before an episode depends on it: a small probe
//! object written under its prefix, read back and removed, which shows the
//! store can be reached, takes Kapto's credentials, and lets it put and
//! read what an upload puts and reads.
//!
//! An upload never deletes anything at a destination, so credentials that
//! may write but not delete are enough: a probe that can't be removed is
//! said so, not counted as a failure.

use object_store::path::Path;
use object_store::{ObjectStore, ObjectStoreExt, PutPayload};

use kapto_config::UploadDestination;
use kapto_core::clock::utc_now_ns;
use kapto_core::{ErrorCode, KaptoError};

use crate::episode::store_error;
use crate::stores;

/// What a probe's name starts with, so one left behind is recognised.
pub const PROBE_PREFIX: &str = ".kapto-probe-";

/// How a probe went, once it was written and read back.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Probed {
    /// Where it was written.
    pub key: String,
    /// Whether it could be removed again; if not, it is still there.
    pub removed: bool,
}

/// Writes a probe named after `name` under `prefix`, reads it back and
/// removes it.
///
/// # Errors
///
/// `upload.unreachable`, `upload.denied` or `upload.failed` as the store
/// answered writing or reading it; `upload.verify_failed` if it read back
/// different.
pub async fn probe(
    store: &dyn ObjectStore,
    prefix: &Path,
    name: &str,
) -> Result<Probed, KaptoError> {
    let key = prefix.clone().join(format!("{PROBE_PREFIX}{name}"));
    let sent = format!("Written by Kapto to test this destination: {name}\n");
    store
        .put(&key, PutPayload::from(sent.clone().into_bytes()))
        .await
        .map_err(|error| store_error(&key, &error))?;
    let read = async { store.get(&key).await?.bytes().await }
        .await
        .map_err(|error| store_error(&key, &error))?;
    if read.as_ref() != sent.as_bytes() {
        return Err(KaptoError::new(ErrorCode::UploadVerifyFailed)
            .detail("the probe read back isn't the one written".to_owned())
            .context("key", key.to_string()));
    }
    let removed = store.delete(&key).await.is_ok();
    Ok(Probed {
        key: key.to_string(),
        removed,
    })
}

/// Probes `destination` under its prefix, with its store opened as an
/// upload opens it, its credentials read through `env`.
///
/// # Errors
///
/// What opening its store reports (`upload.destination_unusable`,
/// `upload.credentials_missing`), and what [`probe`] does.
pub async fn check(
    destination: &UploadDestination,
    env: &(dyn Fn(&str) -> Option<String> + Sync),
) -> Result<Probed, KaptoError> {
    let opened = stores::open(destination, env)?;
    let prefix = Path::parse(&destination.prefix).map_err(|error| {
        KaptoError::new(ErrorCode::UploadDestinationUnusable)
            .detail(format!("the prefix `{}`: {error}", destination.prefix))
    })?;
    let name = format!("{}-{}", utc_now_ns(), std::process::id());
    probe(opened.store().objects, &prefix, &name).await
}

#[cfg(test)]
mod tests {
    use futures::TryStreamExt;
    use object_store::memory::InMemory;

    use super::*;
    use crate::faulty::Faulty;

    fn denied() -> object_store::Error {
        object_store::Error::PermissionDenied {
            path: "fleet-a".to_owned(),
            source: "access denied".into(),
        }
    }

    #[tokio::test]
    async fn a_probe_is_written_read_back_and_removed_under_the_prefix() {
        let store = InMemory::new();
        let probed = probe(&store, &Path::from("fleet-a"), "1").await.unwrap();
        assert_eq!(probed.key, "fleet-a/.kapto-probe-1");
        assert!(probed.removed);
        let left: Vec<_> = store.list(None).try_collect().await.unwrap();
        assert!(left.is_empty(), "{left:?}");
    }

    #[tokio::test]
    async fn a_store_refusing_the_probe_fails_with_its_cause_but_one_refusing_its_removal_doesnt() {
        let store = Faulty::new(InMemory::new());
        store.fail_next(denied());
        let error = probe(&store, &Path::from("fleet-a"), "1")
            .await
            .unwrap_err();
        assert_eq!(error.code(), ErrorCode::UploadDenied);

        // Credentials that may write and read but not delete are enough.
        let refusing = RefusingDeletes(InMemory::new());
        let probed = probe(&refusing, &Path::from("fleet-a"), "3").await.unwrap();
        assert!(!probed.removed, "{probed:?}");
    }

    /// A store that takes and gives back everything, and removes nothing.
    #[derive(Debug)]
    struct RefusingDeletes(InMemory);

    impl std::fmt::Display for RefusingDeletes {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("RefusingDeletes")
        }
    }

    #[async_trait::async_trait]
    impl ObjectStore for RefusingDeletes {
        async fn put_opts(
            &self,
            location: &Path,
            payload: PutPayload,
            options: object_store::PutOptions,
        ) -> object_store::Result<object_store::PutResult> {
            self.0.put_opts(location, payload, options).await
        }

        async fn put_multipart_opts(
            &self,
            location: &Path,
            options: object_store::PutMultipartOptions,
        ) -> object_store::Result<Box<dyn object_store::MultipartUpload>> {
            self.0.put_multipart_opts(location, options).await
        }

        async fn get_opts(
            &self,
            location: &Path,
            options: object_store::GetOptions,
        ) -> object_store::Result<object_store::GetResult> {
            self.0.get_opts(location, options).await
        }

        fn delete_stream(
            &self,
            _: futures::stream::BoxStream<'static, object_store::Result<Path>>,
        ) -> futures::stream::BoxStream<'static, object_store::Result<Path>> {
            Box::pin(futures::stream::once(async { Err(denied()) }))
        }

        fn list(
            &self,
            prefix: Option<&Path>,
        ) -> futures::stream::BoxStream<'static, object_store::Result<object_store::ObjectMeta>>
        {
            self.0.list(prefix)
        }

        async fn list_with_delimiter(
            &self,
            prefix: Option<&Path>,
        ) -> object_store::Result<object_store::ListResult> {
            self.0.list_with_delimiter(prefix).await
        }

        async fn copy_opts(
            &self,
            from: &Path,
            to: &Path,
            options: object_store::CopyOptions,
        ) -> object_store::Result<()> {
            self.0.copy_opts(from, to, options).await
        }
    }
}
