// SPDX-License-Identifier: Apache-2.0

//! The web UI, embedded in the binary when it was built (see `build.rs`),
//! served on the API's origin.
//!
//! Every page is sent with a Content-Security-Policy that lets it load and
//! connect only to its own origin and the viewer bridge's stream, and frame
//! only the viewer bridge's page; the viewer keeps its own, separate policy.

use std::sync::Arc;

use axum::extract::State;
use axum::http::{HeaderValue, Method, StatusCode, Uri, header};
use axum::response::{IntoResponse, Response};

use crate::api::ApiState;

mod files {
    include!(concat!(env!("OUT_DIR"), "/ui_files.rs"));
}

/// The page served when the UI wasn't built into this binary.
const MISSING: &str = include_str!("ui_missing.html");

/// Whether this binary includes the web UI.
#[must_use]
pub fn built() -> bool {
    files::BUILT
}

/// The policy for the UI's pages. The live view reads the bridge's stream
/// on its own port, on whichever host the page was reached by.
fn content_security_policy(stream_port: u16) -> String {
    format!(
        "default-src 'none'; script-src 'self'; style-src 'self'; img-src 'self' data:; \
         font-src 'self'; connect-src 'self' ws://*:{stream_port} wss://*:{stream_port}; \
         base-uri 'none'; form-action 'none'; frame-ancestors 'none'"
    )
}

/// Serves the UI's file at the request's path; `/` is its `index.html`.
pub(crate) async fn file(State(state): State<Arc<ApiState>>, method: Method, uri: Uri) -> Response {
    if method != Method::GET && method != Method::HEAD {
        return StatusCode::METHOD_NOT_ALLOWED.into_response();
    }
    let path = match uri.path() {
        "/" => "/index.html",
        path => path,
    };
    let body: &'static [u8] = if files::BUILT {
        match files::FILES.iter().find(|(served, _)| *served == path) {
            Some((_, body)) => body,
            None => return StatusCode::NOT_FOUND.into_response(),
        }
    } else if path == "/index.html" {
        MISSING.as_bytes()
    } else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let mut response = body.into_response();
    let headers = response.headers_mut();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static(content_type(path)),
    );
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    headers.insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("no-referrer"),
    );
    // Built files carry a hash of their content in their names, so they can
    // be kept; the page that names them must always be fetched again.
    let cache = if path.starts_with("/assets/") {
        "public, max-age=31536000, immutable"
    } else {
        "no-cache"
    };
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static(cache));
    if let Ok(policy) = HeaderValue::from_str(&content_security_policy(state.viewer_stream_port)) {
        headers.insert(header::CONTENT_SECURITY_POLICY, policy);
    }
    response
}

fn content_type(path: &str) -> &'static str {
    match path.rsplit_once('.').map(|(_, extension)| extension) {
        Some("html") => "text/html; charset=utf-8",
        Some("js") => "text/javascript; charset=utf-8",
        Some("css") => "text/css; charset=utf-8",
        Some("svg") => "image/svg+xml",
        Some("png") => "image/png",
        Some("ico") => "image/x-icon",
        Some("json") => "application/json",
        Some("woff2") => "font/woff2",
        _ => "application/octet-stream",
    }
}
