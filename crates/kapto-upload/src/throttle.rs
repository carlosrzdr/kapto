// SPDX-License-Identifier: Apache-2.0

//! A store asking for fewer requests, and how many the engine keeps in
//! flight because of it.
//!
//! With its own retries off, `object_store` gives a throttled answer only as
//! its generic error, with the status in a type it keeps private and no
//! headers; [`throttled`] tells it from the error's words, which the tests
//! below pin by having `object_store`'s own clients answered as each
//! provider throttles. [`Window`] is how many requests may be in flight: it
//! grows by one for each window of requests that succeed, up to the most
//! allowed, and halves when the store throttles, as TCP shares a link.

use object_store::Error;

/// Whether `error` is the store asking for fewer requests: a `429` or
/// `503` answer, or S3's `SlowDown` in any answer.
#[must_use]
pub fn throttled(error: &Error) -> bool {
    if !matches!(error, Error::Generic { .. }) {
        return false;
    }
    let mut words = error.to_string();
    let mut source = std::error::Error::source(error);
    while let Some(cause) = source {
        words.push_str(&cause.to_string());
        source = cause.source();
    }
    ["status code: 429", "status code: 503", "SlowDown"]
        .iter()
        .any(|said| words.contains(said))
}

/// How many requests may be in flight.
#[derive(Clone, Debug)]
pub struct Window {
    limit: usize,
    most: usize,
    /// Requests that succeeded since the limit last changed.
    succeeded: usize,
    /// Changed each time the limit is halved, so requests throttled
    /// together, all sent before it, halve it only once.
    generation: u64,
}

impl Window {
    /// A window starting at `start` requests, never more than `most` nor
    /// fewer than one.
    #[must_use]
    pub fn new(start: usize, most: usize) -> Self {
        let most = most.max(1);
        Self {
            limit: start.clamp(1, most),
            most,
            succeeded: 0,
            generation: 0,
        }
    }

    /// How many requests may be in flight now.
    #[must_use]
    pub fn limit(&self) -> usize {
        self.limit
    }

    /// What a request sent now carries, to say when it was sent.
    #[must_use]
    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// A request succeeded: a whole window of them grows it by one.
    pub fn succeeded(&mut self) {
        self.succeeded += 1;
        if self.succeeded >= self.limit {
            self.limit = (self.limit + 1).min(self.most);
            self.succeeded = 0;
        }
    }

    /// A request sent at `generation` was throttled: halves the window,
    /// unless another sent with it already did.
    pub fn throttled(&mut self, generation: u64) {
        if generation == self.generation {
            self.limit = (self.limit / 2).max(1);
            self.succeeded = 0;
            self.generation += 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::Duration;

    use object_store::aws::AmazonS3Builder;
    use object_store::azure::MicrosoftAzureBuilder;
    use object_store::gcp::GoogleCloudStorageBuilder;
    use object_store::path::Path;
    use object_store::{ClientOptions, ObjectStore, ObjectStoreExt, PutPayload, RetryConfig};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    use super::*;

    /// A stand-in server on loopback answering every request with
    /// `status` and `body`, after reading it whole; its address.
    async fn answering(status: &'static str, body: &'static str) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move {
            while let Ok((mut connection, _)) = listener.accept().await {
                tokio::spawn(async move {
                    let mut request = Vec::new();
                    let mut buffer = [0; 4096];
                    // The headers, then as much body as they say.
                    let body_at = loop {
                        let read = connection.read(&mut buffer).await.unwrap_or(0);
                        if read == 0 {
                            return;
                        }
                        request.extend_from_slice(&buffer[..read]);
                        if let Some(at) = request.windows(4).position(|w| w == b"\r\n\r\n") {
                            break at + 4;
                        }
                    };
                    let headers = String::from_utf8_lossy(&request[..body_at]).to_lowercase();
                    let length: usize = headers
                        .lines()
                        .find_map(|line| line.strip_prefix("content-length:"))
                        .and_then(|value| value.trim().parse().ok())
                        .unwrap_or(0);
                    while request.len() < body_at + length {
                        let read = connection.read(&mut buffer).await.unwrap_or(0);
                        if read == 0 {
                            break;
                        }
                        request.extend_from_slice(&buffer[..read]);
                    }
                    let answer = format!(
                        "HTTP/1.1 {status}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                        body.len()
                    );
                    let _ = connection.write_all(answer.as_bytes()).await;
                });
            }
        });
        format!("http://{address}")
    }

    fn client() -> ClientOptions {
        ClientOptions::new()
            .with_allow_http(true)
            .with_timeout(Duration::from_secs(5))
    }

    fn no_retries() -> RetryConfig {
        RetryConfig {
            max_retries: 0,
            retry_timeout: Duration::ZERO,
            ..RetryConfig::default()
        }
    }

    async fn put_to(store: Arc<dyn ObjectStore>) -> Error {
        store
            .put(&Path::from("probe"), PutPayload::from_static(b"data"))
            .await
            .unwrap_err()
    }

    async fn s3(endpoint: &str) -> Error {
        let store = AmazonS3Builder::new()
            .with_bucket_name("robot-data")
            .with_region("us-east-1")
            .with_endpoint(endpoint)
            .with_skip_signature(true)
            .with_client_options(client())
            .with_retry(no_retries())
            .build()
            .unwrap();
        put_to(Arc::new(store)).await
    }

    async fn gcs(endpoint: &str) -> Error {
        let store = GoogleCloudStorageBuilder::new()
            .with_bucket_name("robot-data")
            .with_base_url(endpoint)
            // Skipping the signature still asks the cloud's metadata server
            // for a token, which a test must never reach.
            .with_bearer_token("kapto-test")
            .with_client_options(client())
            .with_retry(no_retries())
            .build()
            .unwrap();
        put_to(Arc::new(store)).await
    }

    async fn azure(endpoint: &str) -> Error {
        let store = MicrosoftAzureBuilder::new()
            .with_account("kapto")
            .with_container_name("robot-data")
            .with_endpoint(endpoint.to_owned())
            .with_skip_signature(true)
            .with_client_options(client())
            .with_retry(no_retries())
            .build()
            .unwrap();
        put_to(Arc::new(store)).await
    }

    const SLOW_DOWN: &str = "<?xml version=\"1.0\" encoding=\"UTF-8\"?><Error><Code>SlowDown</Code>\
        <Message>Please reduce your request rate.</Message></Error>";
    const GCS_TOO_MANY: &str = "{\"error\":{\"code\":429,\"message\":\"The rate of change requests \
        to the object exceeds the rate limit.\"}}";
    const AZURE_BUSY: &str = "<?xml version=\"1.0\" encoding=\"utf-8\"?><Error><Code>ServerBusy\
        </Code><Message>Ingress is over the account limit.</Message></Error>";

    #[tokio::test]
    async fn each_providers_throttling_as_object_store_reports_it_is_told_apart() {
        let slow_down = answering("503 Slow Down", SLOW_DOWN).await;
        let too_many = answering("429 Too Many Requests", GCS_TOO_MANY).await;
        let busy = answering("503 Server Busy", AZURE_BUSY).await;
        for (provider, error) in [
            ("S3", s3(&slow_down).await),
            ("GCS", gcs(&too_many).await),
            ("Azure", azure(&busy).await),
        ] {
            assert!(throttled(&error), "{provider}: {error}");
        }
    }

    #[tokio::test]
    async fn a_store_refusing_or_failing_otherwise_is_not_throttling() {
        let denied = answering("403 Forbidden", "<Error><Code>AccessDenied</Code></Error>").await;
        let broken = answering(
            "500 Internal Server Error",
            "<Error><Code>InternalError</Code></Error>",
        )
        .await;
        for error in [s3(&denied).await, s3(&broken).await, azure(&broken).await] {
            assert!(!throttled(&error), "{error}");
        }
        // Nothing listening there: unreachable, not throttling.
        let closed = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = format!("http://{}", closed.local_addr().unwrap());
        drop(closed);
        let error = s3(&address).await;
        assert!(!throttled(&error), "{error}");
    }

    #[test]
    fn a_window_grows_by_one_a_window_of_successes_and_halves_once_when_throttled_together() {
        let mut window = Window::new(4, 32);
        for _ in 0..4 {
            window.succeeded();
        }
        assert_eq!(window.limit(), 5);
        // Five sent together, all throttled: halved once, not five times.
        let sent = window.generation();
        for _ in 0..5 {
            window.throttled(sent);
        }
        assert_eq!(window.limit(), 2);
        // Never below one, never above the most.
        let mut small = Window::new(1, 2);
        small.throttled(small.generation());
        assert_eq!(small.limit(), 1);
        for _ in 0..10 {
            small.succeeded();
        }
        assert_eq!(small.limit(), 2);
        assert_eq!(Window::new(64, 32).limit(), 32);
        assert_eq!(Window::new(0, 0).limit(), 1);
    }
}
