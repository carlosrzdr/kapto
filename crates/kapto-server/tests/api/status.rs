// SPDX-License-Identifier: Apache-2.0

//! What Kapto is doing and runs with, and the OpenAPI document.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use axum::http::StatusCode;
use kapto_core::ErrorCode;
use kapto_server::Event;
use kapto_server::api::{ApiState, openapi_json};
use kapto_server::settings::{Setting, SettingsOverview};
use kapto_testkit::scratch;
use serde_json::{Value, json};

use crate::common::{FakeCapture, call, finalised_episode, recording, state};

#[tokio::test]
async fn the_version_names_kapto_and_the_api() {
    let folder = scratch("version");
    let (status, body) = call(state(&folder), "GET", "/api/version", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, json!({"kapto": env!("CARGO_PKG_VERSION"), "api": 1}));
}

#[tokio::test]
async fn the_status_is_capture_s_with_its_newest_values_pre_flight_and_where_the_viewer_is() {
    let folder = scratch("status");
    let _capture = FakeCapture::start(&folder.join("capture.sock"), recording);
    let (status, body) = call(state(&folder), "GET", "/api/status", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        body,
        json!({
            "capture": {
                "episode": "20260310T120000.000Z-1a2b3c4d",
                "seconds": 7,
                "streams": [
                    {"stream": "arm_joints", "received": 14000, "written": 13990, "dropped_by_us": 10, "lost_upstream": 2},
                    {
                        "stream": "wrist_camera", "received": 210, "written": 210, "dropped_by_us": 0, "lost_upstream": 0,
                        "live_view": {"offered": 200, "skipped": 1, "superseded": 30, "not_sent": 4, "sent": 165}
                    }
                ],
                "values": [{"stream": "arm_joints", "values": [0.5, -1.25, 0.04], "age_ms": 12}],
                "preflight": {
                    "ran_at": "2026-03-15T20:00:00.000Z",
                    "window_ms": 5000,
                    "passed": false,
                    "checks": [
                        {
                            "code": "preflight.disk_too_small",
                            "summary": ErrorCode::PreflightDiskTooSmall.summary(),
                            "remedy": ErrorCode::PreflightDiskTooSmall.remedy(),
                            "stream": null,
                            "passed": true,
                            "measured": "measured",
                            "needed": "needed",
                            "waived": false
                        },
                        {
                            "code": "preflight.stream_silent",
                            "summary": ErrorCode::PreflightStreamSilent.summary(),
                            "remedy": ErrorCode::PreflightStreamSilent.remedy(),
                            "stream": "arm_joints",
                            "passed": false,
                            "measured": "measured",
                            "needed": "needed",
                            "waived": true
                        }
                    ]
                }
            },
            "viewer": {"stream_port": 9877}
        })
    );
}

#[tokio::test]
async fn without_a_capture_process_the_status_says_so_with_its_cause_and_remedy() {
    let folder = scratch("unreachable");
    let (status, body) = call(state(&folder), "GET", "/api/status", None).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(body["code"], "capture.control_unreachable");
    assert_eq!(
        body["remedy"],
        ErrorCode::CaptureControlUnreachable.remedy()
    );
}

#[tokio::test]
async fn processes_are_listed_even_while_capture_is_down_with_how_it_ended() {
    let folder = scratch("processes");
    let state = state(&folder);
    state.processes.record(&Event::Started {
        name: "viz-bridge".into(),
        pid: 4243,
    });
    state.processes.record(&Event::Restarting {
        name: "capture".into(),
        reason: "signal: 9 (SIGKILL)".into(),
        delay: Duration::from_secs(4),
    });
    // No capture process answers, yet the processes are known.
    let (status, body) = call(state, "GET", "/api/processes", None).await;
    assert_eq!(status, StatusCode::OK);
    let names: Vec<&str> = body
        .as_array()
        .unwrap()
        .iter()
        .map(|process| process["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["serve", "capture", "viz-bridge"]);
    assert_eq!(body[0]["state"], "running");
    assert_eq!(body[0]["pid"], std::process::id());
    assert_eq!(body[1]["state"], "restarting");
    assert_eq!(body[1]["pid"], Value::Null);
    assert_eq!(body[1]["restarts"], 1);
    assert_eq!(body[1]["last_exit"], "signal: 9 (SIGKILL)");
    assert!(body[1]["restart_in_s"].as_f64().unwrap() > 3.0);
    assert_eq!(body[2]["state"], "running");
    assert_eq!(body[2]["pid"], 4243);
}

#[tokio::test]
async fn the_settings_are_what_serve_started_with_with_their_sources() {
    let folder = scratch("settings");
    let state = ApiState {
        settings: Arc::new(SettingsOverview {
            file: "/config/kapto.toml".into(),
            file_found: true,
            settings: vec![
                Setting {
                    key: "server.port".into(),
                    value: "7701".into(),
                    source: "/config/kapto.toml:2".into(),
                    unit: None,
                    default: false,
                    help: "Port of the API and web UI.".into(),
                },
                Setting {
                    key: "viewer.history_age".into(),
                    value: "600".into(),
                    source: "default".into(),
                    unit: Some("s".into()),
                    default: true,
                    help: "How far back the live view can look.".into(),
                },
            ],
        }),
        ..state(&folder)
    };
    let (status, body) = call(state, "GET", "/api/settings", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["file_found"], true);
    assert_eq!(body["settings"][0]["key"], "server.port");
    assert_eq!(body["settings"][0]["source"], "/config/kapto.toml:2");
    // A quantity says its unit; a port is none.
    assert_eq!(body["settings"][0]["unit"], Value::Null);
    assert_eq!(body["settings"][1]["unit"], "s");
}

/// The bytes in the files of `folder`, which holds no folders.
fn files_bytes(folder: &Path) -> u64 {
    std::fs::read_dir(folder)
        .unwrap()
        .map(|entry| entry.unwrap().metadata().unwrap().len())
        .sum()
}

#[tokio::test]
async fn storage_says_how_much_room_is_left_and_what_episodes_take() {
    let folder = scratch("storage");
    let data = folder.join("data");
    finalised_episode(&data, "20260310T120000.000Z-00000001", 0);
    finalised_episode(&data, "20260310T120500.000Z-00000002", 300);
    // An episode being recorded.
    let recording = data.join(".recording/20260310T121000.000Z-00000003");
    std::fs::create_dir_all(&recording).unwrap();
    std::fs::write(recording.join("recording.mcap"), vec![0; 4096]).unwrap();

    let (status, body) = call(state(&folder), "GET", "/api/storage", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["data_dir"], data.display().to_string());
    assert_eq!(body["episodes"], 2);
    let episodes = files_bytes(&data.join("20260310T120000.000Z-00000001"))
        + files_bytes(&data.join("20260310T120500.000Z-00000002"));
    assert_eq!(body["episodes_bytes"], episodes);
    assert_eq!(body["staging_bytes"], 4096);
    assert_eq!(body["warn_below_bytes"], 10_u64 << 30);
    assert_eq!(body["stop_below_bytes"], 2_u64 << 30);
    let (total, free) = (
        body["total_bytes"].as_u64().unwrap(),
        body["free_bytes"].as_u64().unwrap(),
    );
    assert!(total > 0 && free <= total, "{body}");
}

#[tokio::test]
async fn storage_of_a_data_directory_that_isnt_there_is_an_error_with_its_cause() {
    let folder = scratch("storage-missing");
    // Serve creates it when it starts; here it was removed since.
    let state = state(&folder);
    std::fs::remove_dir_all(&state.data_dir).unwrap();
    let (status, body) = call(state, "GET", "/api/storage", None).await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(body["code"], "server.storage_unreadable");
    assert_eq!(body["remedy"], ErrorCode::ServerStorageUnreadable.remedy());
}

#[test]
fn the_openapi_document_describes_every_route_and_its_error_body() {
    let document: Value = serde_json::from_str(&openapi_json().unwrap()).unwrap();
    let mut paths: Vec<&str> = document["paths"]
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    paths.sort_unstable();
    assert_eq!(
        paths,
        [
            "/api/episodes",
            "/api/episodes/abort",
            "/api/episodes/events",
            "/api/episodes/start",
            "/api/episodes/stop",
            "/api/episodes/{id}",
            "/api/episodes/{id}/cameras/{camera}/frames",
            "/api/episodes/{id}/cameras/{camera}/frames/{n}",
            "/api/episodes/{id}/events",
            "/api/episodes/{id}/qa",
            "/api/episodes/{id}/replay",
            "/api/episodes/{id}/replay/meshes/{n}",
            "/api/episodes/{id}/replay/opening",
            "/api/episodes/{id}/replay/scene",
            "/api/episodes/{id}/replay/window",
            "/api/episodes/{id}/restore",
            "/api/episodes/{id}/statistics",
            "/api/episodes/{id}/uploads",
            "/api/episodes/{id}/uploads/retry",
            "/api/event-kinds",
            "/api/events",
            "/api/layout",
            "/api/layout/check",
            "/api/preflight",
            "/api/processes",
            "/api/profiles",
            "/api/robot",
            "/api/robot/meshes/{n}",
            "/api/robot/scene",
            "/api/settings",
            "/api/status",
            "/api/storage",
            "/api/tasks",
            "/api/uploads",
            "/api/uploads/destinations/{name}/test",
            "/api/version"
        ]
    );
    let conflict = &document["paths"]["/api/episodes/stop"]["post"]["responses"]["409"];
    assert_eq!(
        conflict["content"]["application/json"]["schema"]["$ref"],
        "#/components/schemas/ApiError"
    );
}
