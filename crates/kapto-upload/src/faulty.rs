// SPDX-License-Identifier: Apache-2.0

//! A store that fails as a test says: wrapped around any `object_store`
//! store, it answers its next calls with the errors a test queued, then
//! passes calls on again, so the engine can be checked against a link that
//! drops, credentials that stop working or a provider that throttles,
//! without a server. It can also corrupt what is put at a key, keeping its
//! size, as a faulty disk or link would, so verification can be checked
//! to notice.

use std::collections::VecDeque;
use std::fmt;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use async_trait::async_trait;
use futures::stream::{self, BoxStream, StreamExt};
use object_store::UploadPart;
use object_store::multipart::{MultipartStore, PartId};
use object_store::path::Path;
use object_store::{
    CopyOptions, Error, GetOptions, GetResult, ListResult, MultipartId, MultipartUpload,
    ObjectMeta, ObjectStore, PutMultipartOptions, PutOptions, PutPayload, PutResult, Result,
};

/// `store`, failing its next calls with the errors queued by
/// [`Faulty::fail_next`].
#[derive(Debug)]
pub struct Faulty<S> {
    store: S,
    faults: Mutex<VecDeque<Error>>,
    /// What the multipart uploads it hands out fail with, part or completion.
    part_faults: Arc<Mutex<VecDeque<Error>>>,
    /// How many multipart uploads were aborted.
    aborted: Arc<AtomicUsize>,
    /// Where what is put is stored with its first byte changed.
    corrupted: Mutex<Vec<Path>>,
    /// Each location read, once per read, to show what was read back.
    reads: Mutex<Vec<Path>>,
}

impl<S> Faulty<S> {
    /// `store`, failing nothing yet.
    pub fn new(store: S) -> Self {
        Self {
            store,
            faults: Mutex::new(VecDeque::new()),
            part_faults: Arc::new(Mutex::new(VecDeque::new())),
            aborted: Arc::new(AtomicUsize::new(0)),
            corrupted: Mutex::new(Vec::new()),
            reads: Mutex::new(Vec::new()),
        }
    }

    /// Stores what is put whole at `location` from now on with its first
    /// byte changed: the same size, other contents.
    pub fn corrupt_puts_to(&self, location: Path) {
        self.corrupted
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(location);
    }

    /// How many reads of `location`'s contents, whole or by range, it
    /// passed on.
    pub fn reads_of(&self, location: &Path) -> usize {
        self.reads
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .filter(|read| *read == location)
            .count()
    }

    /// Makes the next call fail with `error`, after those queued before.
    pub fn fail_next(&self, error: Error) {
        lock(&self.faults).push_back(error);
    }

    /// Makes the next part put, or completion, of a multipart upload it
    /// handed out fail with `error`, after those queued before.
    pub fn fail_next_part(&self, error: Error) {
        lock(&self.part_faults).push_back(error);
    }

    /// How many multipart uploads it handed out were aborted.
    pub fn aborted(&self) -> usize {
        self.aborted.load(Ordering::Relaxed)
    }

    /// The error queued for this call, if any.
    fn fault(&self) -> Result<()> {
        fault(&self.faults)
    }
}

fn lock(faults: &Mutex<VecDeque<Error>>) -> std::sync::MutexGuard<'_, VecDeque<Error>> {
    faults.lock().unwrap_or_else(PoisonError::into_inner)
}

fn fault(faults: &Mutex<VecDeque<Error>>) -> Result<()> {
    lock(faults).pop_front().map_or(Ok(()), Err)
}

/// A multipart upload of the store's, failing its next part or completion
/// with the errors queued, and counting its abort.
#[derive(Debug)]
struct FaultyUpload {
    upload: Box<dyn MultipartUpload>,
    faults: Arc<Mutex<VecDeque<Error>>>,
    aborted: Arc<AtomicUsize>,
}

#[async_trait]
impl MultipartUpload for FaultyUpload {
    fn put_part(&mut self, data: PutPayload) -> UploadPart {
        match fault(&self.faults) {
            Ok(()) => self.upload.put_part(data),
            Err(error) => Box::pin(async move { Err(error) }),
        }
    }

    async fn complete(&mut self) -> Result<PutResult> {
        fault(&self.faults)?;
        self.upload.complete().await
    }

    async fn abort(&mut self) -> Result<()> {
        self.aborted.fetch_add(1, Ordering::Relaxed);
        self.upload.abort().await
    }
}

impl<S: fmt::Display> fmt::Display for Faulty<S> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Faulty({})", self.store)
    }
}

#[async_trait]
impl<S: ObjectStore> ObjectStore for Faulty<S> {
    async fn put_opts(
        &self,
        location: &Path,
        payload: PutPayload,
        options: PutOptions,
    ) -> Result<PutResult> {
        self.fault()?;
        let corrupt = self
            .corrupted
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .contains(location);
        if !corrupt {
            return self.store.put_opts(location, payload, options).await;
        }
        let mut bytes: Vec<u8> = payload.iter().flat_map(|chunk| chunk.to_vec()).collect();
        if let Some(first) = bytes.first_mut() {
            *first ^= 0xFF;
        }
        self.store.put_opts(location, bytes.into(), options).await
    }

    async fn put_multipart_opts(
        &self,
        location: &Path,
        options: PutMultipartOptions,
    ) -> Result<Box<dyn MultipartUpload>> {
        self.fault()?;
        let upload = self.store.put_multipart_opts(location, options).await?;
        Ok(Box::new(FaultyUpload {
            upload,
            faults: Arc::clone(&self.part_faults),
            aborted: Arc::clone(&self.aborted),
        }))
    }

    async fn get_opts(&self, location: &Path, options: GetOptions) -> Result<GetResult> {
        self.fault()?;
        // A head request asks for the metadata only, reading nothing.
        if !options.head {
            self.reads
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push(location.clone());
        }
        self.store.get_opts(location, options).await
    }

    fn delete_stream(
        &self,
        locations: BoxStream<'static, Result<Path>>,
    ) -> BoxStream<'static, Result<Path>> {
        match self.fault() {
            Ok(()) => self.store.delete_stream(locations),
            Err(error) => stream::once(async { Err(error) }).boxed(),
        }
    }

    fn list(&self, prefix: Option<&Path>) -> BoxStream<'static, Result<ObjectMeta>> {
        match self.fault() {
            Ok(()) => self.store.list(prefix),
            Err(error) => stream::once(async { Err(error) }).boxed(),
        }
    }

    async fn list_with_delimiter(&self, prefix: Option<&Path>) -> Result<ListResult> {
        self.fault()?;
        self.store.list_with_delimiter(prefix).await
    }

    async fn copy_opts(&self, from: &Path, to: &Path, options: CopyOptions) -> Result<()> {
        self.fault()?;
        self.store.copy_opts(from, to, options).await
    }
}

#[async_trait]
impl<S: MultipartStore> MultipartStore for Faulty<S> {
    async fn create_multipart(&self, path: &Path) -> Result<MultipartId> {
        self.fault()?;
        self.store.create_multipart(path).await
    }

    async fn put_part(
        &self,
        path: &Path,
        id: &MultipartId,
        part_idx: usize,
        data: PutPayload,
    ) -> Result<PartId> {
        self.fault()?;
        self.store.put_part(path, id, part_idx, data).await
    }

    async fn complete_multipart(
        &self,
        path: &Path,
        id: &MultipartId,
        parts: Vec<PartId>,
    ) -> Result<PutResult> {
        self.fault()?;
        self.store.complete_multipart(path, id, parts).await
    }

    async fn abort_multipart(&self, path: &Path, id: &MultipartId) -> Result<()> {
        self.fault()?;
        self.store.abort_multipart(path, id).await
    }
}
