// SPDX-License-Identifier: Apache-2.0

//! S3-compatible storage for the tests: the RustFS server
//! `cargo xtask storage` starts, at the address and with the credentials it
//! sets in the tests' environment. A test that needs it is ignored by the
//! other checks and asks for a bucket of its own, so tests never see each
//! other's objects.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "a test's setup fails loudly, saying what is missing"
)]

use std::time::Duration;

use object_store::aws::{AmazonS3, AmazonS3Builder, Checksum};
use object_store::path::Path;
use object_store::signer::{Method, Signer};

/// The server's address, e.g. `http://127.0.0.1:39000`.
pub const ENDPOINT: &str = "KAPTO_TEST_S3_ENDPOINT";

/// The server's address, as `cargo xtask storage` sets it.
///
/// # Panics
///
/// When the test isn't run by `cargo xtask storage`.
#[must_use]
pub fn endpoint() -> String {
    std::env::var(ENDPOINT).unwrap_or_else(|_| {
        panic!("{ENDPOINT} isn't set: run the storage tests with `cargo xtask storage`")
    })
}

/// A new, empty bucket `name` on the server, and a store for it set up as
/// the engine's S3 stores are: checking each file's SHA-256, with the
/// credentials from the environment.
///
/// # Panics
///
/// When the server can't be reached or refuses the bucket.
pub async fn bucket(name: &str) -> AmazonS3 {
    let store = AmazonS3Builder::from_env()
        .with_endpoint(endpoint())
        .with_allow_http(true)
        .with_bucket_name(name)
        .with_checksum_algorithm(Checksum::SHA256)
        .build()
        .unwrap();
    // A presigned PUT of the bucket's root is S3's CreateBucket.
    let url = store
        .signed_url(Method::PUT, &Path::from(""), Duration::from_secs(60))
        .await
        .unwrap();
    let status = send(Method::PUT, url.as_str(), &[], Vec::new()).await;
    assert!(
        status.is_success() || status.as_u16() == 409,
        "creating the bucket `{name}`: {status}"
    );
    store
}

/// Sends a request to a presigned `url` with `headers` and `body`, and
/// returns the server's status.
///
/// # Panics
///
/// When the server can't be reached.
pub async fn send(
    method: Method,
    url: &str,
    headers: &[(&str, String)],
    body: Vec<u8>,
) -> reqwest::StatusCode {
    let url = url.to_owned();
    let headers: Vec<(String, String)> = headers
        .iter()
        .map(|(name, value)| ((*name).to_owned(), value.clone()))
        .collect();
    // The blocking client runs its own runtime, which mustn't be inside
    // the test's.
    tokio::task::spawn_blocking(move || {
        let mut request = reqwest::blocking::Client::new()
            .request(method, url)
            .body(body);
        for (name, value) in headers {
            request = request.header(name, value);
        }
        request.send().unwrap().status()
    })
    .await
    .unwrap()
}

#[cfg(test)]
mod tests {
    use object_store::ObjectStoreExt as _;
    use object_store::multipart::MultipartStore as _;
    use object_store::signer::{HeaderName, HeaderValue, SignedUrlOptions};

    use super::*;

    #[tokio::test]
    #[ignore = "needs the RustFS server `cargo xtask storage` starts"]
    async fn s3_compatible_storage_does_what_the_engine_relies_on() {
        let store = bucket("conformance").await;
        crate::conformance::check(&store, Some(&store), &Path::from("conformance"), 5 << 20).await;
    }

    #[tokio::test]
    #[ignore = "needs the RustFS server `cargo xtask storage` starts"]
    async fn s3_refuses_a_file_or_a_part_whose_sha256_isnt_the_one_sent() {
        let store = bucket("checksums").await;
        let sent = vec![7_u8; 1000];
        // The SHA-256 of nothing, in base64 as S3 takes it: wrong for what
        // is sent.
        let other = "47DEQpj8HBSa+/TImW+5JCeuQeRkm5NMpJWZG3hSuFU=";
        let wrong = [("x-amz-checksum-sha256", other.to_owned())];
        let file = Path::from("episode/recording.mcap");
        let signed = |query: Vec<(&'static str, String)>| {
            SignedUrlOptions::default()
                .with_query(query)
                .with_signed_header(
                    HeaderName::from_static("x-amz-checksum-sha256"),
                    HeaderValue::from_static(other),
                )
        };

        let url = store
            .signed_url_opts(
                Method::PUT,
                &file,
                Duration::from_secs(60),
                &signed(Vec::new()),
            )
            .await
            .unwrap();
        // Signed as it is, the request is taken when the checksum is right:
        // a refusal below is the checksum's.
        let status = send(Method::PUT, url.as_str(), &wrong, Vec::new()).await;
        assert!(
            status.is_success(),
            "nothing, with its own checksum: {status}"
        );
        store.delete(&file).await.unwrap();
        let status = send(Method::PUT, url.as_str(), &wrong, sent.clone()).await;
        assert_eq!(status.as_u16(), 400, "a whole file");
        assert!(store.head(&file).await.is_err(), "a refused file is there");

        let upload = store.create_multipart(&file).await.unwrap();
        let part = signed(vec![
            ("partNumber", "1".to_owned()),
            ("uploadId", upload.clone()),
        ]);
        let url = store
            .signed_url_opts(Method::PUT, &file, Duration::from_secs(60), &part)
            .await
            .unwrap();
        let status = send(Method::PUT, url.as_str(), &wrong, sent).await;
        assert_eq!(status.as_u16(), 400, "a part");
        store.abort_multipart(&file, &upload).await.unwrap();
    }
}
