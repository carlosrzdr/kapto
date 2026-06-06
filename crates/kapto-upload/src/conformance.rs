// SPDX-License-Identifier: Apache-2.0

//! What the upload engine relies on a store for, checked the same way for
//! every kind of store it is configured with: run [`check`] in the store's
//! own tests, under a prefix no other test uses.
//!
//! The engine puts each file whole or, where the store can, in parts it
//! can carry on with after a restart from the upload and part IDs it saved;
//! puts the completeness marker only if nothing is there, so a finished
//! episode is never overwritten; reads files back by range to verify them;
//! and lists what is under an episode's prefix to find what is already up.

#![allow(
    clippy::unwrap_used,
    clippy::panic,
    reason = "a test suite fails loudly, naming what the store did wrong"
)]

use futures::TryStreamExt;
use object_store::multipart::{MultipartStore, PartId};
use object_store::path::Path;
use object_store::{Error, ObjectStore, ObjectStoreExt, PutMode, PutOptions, PutPayload};

/// Checks `store` under `prefix`, and its multipart uploads if it has them,
/// with parts of `part_bytes` (5 MiB is the smallest S3 takes). Leaves
/// nothing behind.
///
/// # Panics
///
/// At the first thing the store does that the engine can't rely on,
/// saying what.
pub async fn check(
    store: &dyn ObjectStore,
    multipart: Option<&dyn MultipartStore>,
    prefix: &Path,
    part_bytes: usize,
) {
    missing_objects_are_not_found(store, prefix).await;
    a_file_put_whole_reads_back_by_range(store, prefix).await;
    the_marker_is_put_only_if_nothing_is_there(store, prefix).await;
    objects_are_listed_under_their_prefix_only(store, prefix).await;
    if let Some(multipart) = multipart {
        a_file_put_in_parts_carries_on_after_a_restart(store, multipart, prefix, part_bytes).await;
        an_aborted_upload_leaves_nothing(store, multipart, prefix, part_bytes).await;
    }
    let left: Vec<Path> = store
        .list(Some(prefix))
        .map_ok(|o| o.location)
        .try_collect()
        .await
        .unwrap();
    for location in left {
        store.delete(&location).await.unwrap();
    }
    let left = store
        .list(Some(prefix))
        .try_collect::<Vec<_>>()
        .await
        .unwrap();
    assert!(left.is_empty(), "left behind: {left:?}");
}

/// `size` bytes that differ from one place to the next.
fn data(size: usize) -> Vec<u8> {
    (0..=250_u8).cycle().take(size).collect()
}

fn not_found<T: std::fmt::Debug>(result: object_store::Result<T>, what: &str) {
    match result {
        Err(Error::NotFound { .. }) => {}
        other => panic!("{what}: expected NotFound, got {other:?}"),
    }
}

async fn missing_objects_are_not_found(store: &dyn ObjectStore, prefix: &Path) {
    let missing = prefix.clone().join("missing");
    not_found(store.head(&missing).await, "the size of a missing object");
    not_found(
        store.get_range(&missing, 0..1).await,
        "reading a missing object",
    );
}

async fn a_file_put_whole_reads_back_by_range(store: &dyn ObjectStore, prefix: &Path) {
    let location = prefix.clone().join("episode.json");
    let file = data(1000);
    let put = store
        .put(&location, PutPayload::from(file.clone()))
        .await
        .unwrap();
    let meta = store.head(&location).await.unwrap();
    assert_eq!(meta.size, 1000);
    assert!(meta.e_tag.is_some(), "no tag to record the object by");
    assert_eq!(meta.e_tag, put.e_tag, "a tag other than the one put");
    let middle = store.get_range(&location, 10..510).await.unwrap();
    assert_eq!(middle.as_ref(), &file[10..510]);
}

async fn the_marker_is_put_only_if_nothing_is_there(store: &dyn ObjectStore, prefix: &Path) {
    let marker = prefix.clone().join("_SUCCESS");
    let create = || PutOptions::from(PutMode::Create);
    store
        .put_opts(&marker, PutPayload::from_static(b"first"), create())
        .await
        .unwrap();
    match store
        .put_opts(&marker, PutPayload::from_static(b"second"), create())
        .await
    {
        Err(Error::AlreadyExists { .. }) => {}
        other => panic!("putting over the marker: expected AlreadyExists, got {other:?}"),
    }
    let kept = store.get(&marker).await.unwrap().bytes().await.unwrap();
    assert_eq!(kept.as_ref(), b"first", "the marker was overwritten");
}

async fn objects_are_listed_under_their_prefix_only(store: &dyn ObjectStore, prefix: &Path) {
    let episode = prefix.clone().join("episode");
    let beside = prefix.clone().join("episode-2").join("a");
    for location in [episode.clone().join("b"), episode.clone().join("a"), beside] {
        store
            .put(&location, PutPayload::from_static(b"x"))
            .await
            .unwrap();
    }
    let mut listed: Vec<String> = store
        .list(Some(&episode))
        .map_ok(|object| object.location.to_string())
        .try_collect()
        .await
        .unwrap();
    listed.sort();
    assert_eq!(
        listed,
        [episode.clone().join("a"), episode.clone().join("b")].map(|p| p.to_string())
    );
}

async fn a_file_put_in_parts_carries_on_after_a_restart(
    store: &dyn ObjectStore,
    multipart: &dyn MultipartStore,
    prefix: &Path,
    part_bytes: usize,
) {
    let location = prefix.clone().join("recording.mcap");
    let file = data(part_bytes * 2 + part_bytes / 2);
    let parts: Vec<&[u8]> = file.chunks(part_bytes).collect();
    let id = multipart.create_multipart(&location).await.unwrap();
    let first = put_part(multipart, &location, &id, 0, parts[0]).await;
    not_found(store.head(&location).await, "a file not finished");

    // After a restart: only the IDs the engine saved are left.
    let (id, mut done) = saved(&id, &[first]);
    for (index, part) in parts.iter().enumerate().skip(1) {
        done.push(put_part(multipart, &location, &id, index, part).await);
    }
    multipart
        .complete_multipart(&location, &id, done)
        .await
        .unwrap();
    assert_eq!(store.head(&location).await.unwrap().size, file.len() as u64);
    let whole = store.get(&location).await.unwrap().bytes().await.unwrap();
    assert!(
        whole.as_ref() == file.as_slice(),
        "the file came back different"
    );
}

async fn an_aborted_upload_leaves_nothing(
    store: &dyn ObjectStore,
    multipart: &dyn MultipartStore,
    prefix: &Path,
    part_bytes: usize,
) {
    let location = prefix.clone().join("aborted.mcap");
    let id = multipart.create_multipart(&location).await.unwrap();
    put_part(multipart, &location, &id, 0, &data(part_bytes)).await;
    multipart.abort_multipart(&location, &id).await.unwrap();
    not_found(store.head(&location).await, "an aborted file");
}

async fn put_part(
    multipart: &dyn MultipartStore,
    location: &Path,
    id: &str,
    index: usize,
    part: &[u8],
) -> PartId {
    let payload = PutPayload::from(part.to_vec());
    multipart
        .put_part(location, &id.to_owned(), index, payload)
        .await
        .unwrap()
}

/// The upload and part IDs as the engine keeps them between parts: plain
/// text, all that survives a restart.
fn saved(id: &str, parts: &[PartId]) -> (String, Vec<PartId>) {
    let saved: Vec<String> = parts.iter().map(|part| part.content_id.clone()).collect();
    let parts = saved
        .into_iter()
        .map(|content_id| PartId { content_id })
        .collect();
    (id.to_owned(), parts)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use async_trait::async_trait;
    use futures::stream::BoxStream;
    use object_store::local::LocalFileSystem;
    use object_store::memory::InMemory;
    use object_store::{
        CopyOptions, GetOptions, GetResult, ListResult, MultipartUpload, ObjectMeta,
        PutMultipartOptions, PutResult, Result,
    };

    use super::*;
    use crate::faulty::Faulty;

    #[tokio::test]
    async fn the_in_memory_store_does_all_the_engine_relies_on() {
        let store = InMemory::new();
        check(&store, Some(&store), &Path::from("conformance"), 16).await;
    }

    #[tokio::test]
    async fn a_local_folder_does_all_but_parts() {
        let folder = kapto_testkit::scratch("upload-local-folder");
        let store = LocalFileSystem::new_with_prefix(&folder).unwrap();
        check(&store, None, &Path::from("conformance"), 16).await;
    }

    #[tokio::test]
    async fn a_faulty_store_fails_its_next_calls_as_told_then_works_again() {
        let store = Faulty::new(InMemory::new());
        let location = Path::from("robot/episode.json");
        store.fail_next(Error::PermissionDenied {
            path: location.to_string(),
            source: "the key was revoked".into(),
        });
        store.fail_next(Error::Generic {
            store: "test",
            source: "connection reset".into(),
        });
        let payload = || PutPayload::from_static(b"{}");
        assert!(matches!(
            store.put(&location, payload()).await,
            Err(Error::PermissionDenied { .. })
        ));
        assert!(matches!(
            store.create_multipart(&location).await,
            Err(Error::Generic { .. })
        ));
        store.put(&location, payload()).await.unwrap();
        check(&store, Some(&store), &Path::from("conformance"), 16).await;
    }

    /// A store that puts over what is there even when asked not to.
    #[derive(Debug)]
    struct Overwriting(Arc<InMemory>);

    impl std::fmt::Display for Overwriting {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("Overwriting")
        }
    }

    #[async_trait]
    impl ObjectStore for Overwriting {
        async fn put_opts(&self, at: &Path, data: PutPayload, _: PutOptions) -> Result<PutResult> {
            self.0.put(at, data).await
        }
        async fn put_multipart_opts(
            &self,
            at: &Path,
            options: PutMultipartOptions,
        ) -> Result<Box<dyn MultipartUpload>> {
            self.0.put_multipart_opts(at, options).await
        }
        async fn get_opts(&self, at: &Path, options: GetOptions) -> Result<GetResult> {
            self.0.get_opts(at, options).await
        }
        fn delete_stream(
            &self,
            locations: BoxStream<'static, Result<Path>>,
        ) -> BoxStream<'static, Result<Path>> {
            self.0.delete_stream(locations)
        }
        fn list(&self, prefix: Option<&Path>) -> BoxStream<'static, Result<ObjectMeta>> {
            self.0.list(prefix)
        }
        async fn list_with_delimiter(&self, prefix: Option<&Path>) -> Result<ListResult> {
            self.0.list_with_delimiter(prefix).await
        }
        async fn copy_opts(&self, from: &Path, to: &Path, options: CopyOptions) -> Result<()> {
            self.0.copy_opts(from, to, options).await
        }
    }

    #[tokio::test]
    #[should_panic(expected = "putting over the marker: expected AlreadyExists")]
    async fn the_suite_fails_a_store_that_overwrites_the_marker() {
        let store = Overwriting(Arc::new(InMemory::new()));
        check(&store, None, &Path::from("conformance"), 16).await;
    }
}
