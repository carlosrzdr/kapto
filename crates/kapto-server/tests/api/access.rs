// SPDX-License-Identifier: Apache-2.0

//! Who may reach the API and the UI: a JSON body for every change, the UI
//! page's own origin, and this machine's names only while listening here.

use std::net::IpAddr;

use axum::body::Body;
use axum::http::{Request as HttpRequest, StatusCode, header};
use kapto_server::api::{ApiState, router};
use kapto_testkit::scratch;
use serde_json::Value;
use tower::ServiceExt;

use crate::common::{FakeCapture, call, recording, state};

#[tokio::test]
async fn changes_need_a_json_body_so_another_site_cant_send_them_as_a_form() {
    let folder = scratch("forms");
    let _capture = FakeCapture::start(&folder.join("capture.sock"), recording);
    for (uri, body) in [
        ("/api/episodes/start", Some(("text/plain", "{}"))),
        (
            "/api/episodes/start",
            Some(("application/x-www-form-urlencoded", "duration_s=5")),
        ),
        ("/api/episodes/stop", None),
        ("/api/episodes/abort", None),
        ("/api/preflight", None),
        (
            "/api/episodes/events",
            Some(("text/plain", r#"{"kind":"note"}"#)),
        ),
        ("/api/episodes/20260310T120000.000Z-00000001/replay", None),
        (
            "/api/episodes/start",
            Some(("application/json", r#"{"duration":5}"#)),
        ),
    ] {
        let (status, reply) = call(state(&folder), "POST", uri, body).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{uri} {body:?}");
        assert_eq!(reply["code"], "api.invalid_request", "{uri} {body:?}");
    }
}

/// Sends one request and returns the status, one header and the body.
async fn fetch(
    state: ApiState,
    method: &str,
    uri: &str,
    header: &str,
) -> (StatusCode, String, String) {
    let request = HttpRequest::builder()
        .method(method)
        .uri(uri)
        .body(Body::empty())
        .unwrap();
    let response = router(state).oneshot(request).await.unwrap();
    let status = response.status();
    let value = response
        .headers()
        .get(header)
        .map(|value| value.to_str().unwrap().to_owned())
        .unwrap_or_default();
    let bytes = axum::body::to_bytes(response.into_body(), 1 << 22)
        .await
        .unwrap();
    (status, value, String::from_utf8_lossy(&bytes).into_owned())
}

#[tokio::test]
async fn the_ui_page_may_only_reach_its_own_origin_and_the_live_stream_and_frames_nothing() {
    let folder = scratch("ui");
    let (status, policy, body) = fetch(state(&folder), "GET", "/", "content-security-policy").await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        policy.starts_with("default-src 'none'; script-src 'self';"),
        "{policy}"
    );
    assert!(
        policy.contains("connect-src 'self' ws://*:9877 wss://*:9877;"),
        "{policy}"
    );
    assert!(!policy.contains("frame-src"), "{policy}");
    assert!(policy.ends_with("frame-ancestors 'none'"), "{policy}");
    if kapto_server::ui::built() {
        assert!(body.contains(r#"<div id="app">"#), "{body}");
    } else {
        assert!(body.contains("built without its web UI"), "{body}");
    }
    let (_, cache, _) = fetch(state(&folder), "GET", "/", "cache-control").await;
    assert_eq!(cache, "no-cache");
}

#[tokio::test]
async fn files_the_ui_doesnt_have_are_not_found_and_only_reading_is_allowed() {
    let folder = scratch("ui-missing");
    let (status, _, _) = fetch(state(&folder), "GET", "/assets/none.js", "").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _, _) = fetch(state(&folder), "GET", "/../Cargo.toml", "").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _, _) = fetch(state(&folder), "DELETE", "/index.html", "").await;
    assert_eq!(status, StatusCode::METHOD_NOT_ALLOWED);
}

/// The status of a GET to `/api/version` addressed to `host`, from an API
/// listening on `bind`, and the cause code if refused.
async fn version_for_host(bind: [u8; 4], host: &str) -> (StatusCode, Value) {
    let folder = scratch("host");
    let request = HttpRequest::builder()
        .uri("/api/version")
        .header(header::HOST, host)
        .body(Body::empty())
        .unwrap();
    let response = router(ApiState {
        bind: IpAddr::from(bind),
        ..state(&folder)
    })
    .oneshot(request)
    .await
    .unwrap();
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), 1 << 20)
        .await
        .unwrap();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

#[tokio::test]
async fn listening_only_here_it_refuses_other_host_names_against_dns_rebinding() {
    for host in ["localhost:7700", "127.0.0.1:7700", "[::1]:7700"] {
        let (status, _) = version_for_host([127, 0, 0, 1], host).await;
        assert_eq!(status, StatusCode::OK, "{host}");
    }
    let (status, body) = version_for_host([127, 0, 0, 1], "attacker.example:7700").await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(body["code"], "api.host_not_allowed");
    assert!(
        body["message"]
            .as_str()
            .unwrap()
            .contains("attacker.example:7700")
    );
}

#[tokio::test]
async fn listening_on_the_network_it_answers_to_any_host_name() {
    let (status, _) = version_for_host([0, 0, 0, 0], "robot-7.local:7700").await;
    assert_eq!(status, StatusCode::OK);
}
