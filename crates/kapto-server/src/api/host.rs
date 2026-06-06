// SPDX-License-Identifier: Apache-2.0

//! Answering only to this machine's names while listening only on it.
//!
//! A web page on another site can't read the API's answers: the browser
//! keeps them from it. Unless the site's DNS name is made to point to this
//! machine after the page has loaded (DNS rebinding): the browser then sees
//! the same origin and lets the page read everything. Such requests still
//! carry the site's name in their `Host` header, so they are refused here.
//! Listening on the network, the API can be reached under any name; keeping
//! it safe there is up to authentication.

use std::net::IpAddr;
use std::sync::Arc;

use axum::extract::{Request, State};
use axum::http::header;
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use kapto_core::{ErrorCode, KaptoError};

use super::{ApiState, Failure};

/// Refuses a request addressed to another host name while the API listens
/// only on this machine.
pub(super) async fn check(
    State(state): State<Arc<ApiState>>,
    request: Request,
    next: Next,
) -> Response {
    if state.bind.is_loopback()
        && let Some(host) = request.headers().get(header::HOST)
        && !host.to_str().is_ok_and(is_this_machine)
    {
        let host = String::from_utf8_lossy(host.as_bytes()).into_owned();
        return Failure::from(KaptoError::new(ErrorCode::ApiHostNotAllowed).detail(host))
            .into_response();
    }
    next.run(request).await
}

/// Whether a `Host` header names this machine: `localhost` or a loopback
/// address, with any port.
fn is_this_machine(host: &str) -> bool {
    let name = without_port(host);
    name.eq_ignore_ascii_case("localhost")
        || name
            .trim_start_matches('[')
            .trim_end_matches(']')
            .parse::<IpAddr>()
            .is_ok_and(|address| address.is_loopback())
}

/// The host without its port; an IPv6 address keeps its brackets.
pub(super) fn without_port(host: &str) -> &str {
    if host.starts_with('[') {
        return host.find(']').map_or(host, |end| &host[..=end]);
    }
    match host.rsplit_once(':') {
        Some((name, port)) if port.bytes().all(|byte| byte.is_ascii_digit()) => name,
        _ => host,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn this_machine_is_localhost_or_a_loopback_address_with_any_port() {
        for host in [
            "localhost",
            "LocalHost:7700",
            "127.0.0.1",
            "127.0.0.1:7700",
            "127.1.2.3:80",
            "[::1]",
            "[::1]:7700",
        ] {
            assert!(is_this_machine(host), "{host}");
        }
    }

    #[test]
    fn other_names_and_addresses_are_not_this_machine() {
        for host in [
            "attacker.example",
            "attacker.example:7700",
            "localhost.attacker.example",
            "127.0.0.1.nip.io:7700",
            "192.168.1.20:7700",
            "[fe80::1]:7700",
            "",
        ] {
            assert!(!is_this_machine(host), "{host}");
        }
    }
}
