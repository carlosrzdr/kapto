// SPDX-License-Identifier: Apache-2.0

//! Starting, stopping and aborting episodes, pre-flight and events, all
//! through the capture process.

use axum::http::StatusCode;
use kapto_capture::control::{Request, Response, Status};
use kapto_core::{ErrorCode, KaptoError};
use kapto_testkit::scratch;
use serde_json::json;

use crate::common::{FakeCapture, call, idle, recording, saved, silent_arm, state};

/// A capture process that starts only when asked with `lab_ana` for `arm7`
/// and the task `pick_and_place`, as copies of the files.
fn started_with_session(request: &Request) -> Response {
    match request {
        Request::Start {
            profile: Some(chosen),
            task: Some(task),
            ..
        } if chosen.package == "arm7"
            && chosen.profile.id == "lab_ana"
            && chosen.profile.operator.as_deref() == Some("Ana")
            && task.subtasks == ["reach", "grasp"] =>
        {
            Response::Started {
                episode: "20260310T120000.000Z-1a2b3c4d".into(),
            }
        }
        _ => Response::error(&KaptoError::new(ErrorCode::CaptureControlInvalidRequest)),
    }
}

#[tokio::test]
async fn an_episode_is_started_with_copies_of_the_profile_and_task_asked_for() {
    let folder = scratch("start-session");
    let _capture = FakeCapture::start(&folder.join("capture.sock"), started_with_session);
    saved(
        &folder,
        "profiles/lab_ana.toml",
        "package = \"arm7\"\noperator = \"Ana\"\n",
    );
    saved(
        &folder,
        "profiles/broken.toml",
        "package = \"arm7\"\nmode = \"remote\"\n",
    );
    saved(
        &folder,
        "tasks/pick_and_place.toml",
        "subtasks = [\"reach\", \"grasp\"]\n",
    );
    let start = |body: &'static str| {
        let state = state(&folder);
        async move {
            call(
                state,
                "POST",
                "/api/episodes/start",
                Some(("application/json", body)),
            )
            .await
        }
    };
    let (status, _) = start(r#"{"profile":"lab_ana","task":"pick_and_place"}"#).await;
    assert_eq!(status, StatusCode::CREATED);
    let (status, body) = start(r#"{"profile":"lab_ben"}"#).await;
    assert_eq!(
        (status, body["code"].as_str()),
        (StatusCode::NOT_FOUND, Some("profile.not_found"))
    );
    let (status, body) = start(r#"{"task":"stacking"}"#).await;
    assert_eq!(
        (status, body["code"].as_str()),
        (StatusCode::NOT_FOUND, Some("task.not_found"))
    );
    // There, but can't be read: its own problem, not "not found".
    let (status, body) = start(r#"{"profile":"broken"}"#).await;
    assert_eq!(
        (status, body["code"].as_str()),
        (StatusCode::CONFLICT, Some("config.invalid_value"))
    );
}

#[tokio::test]
async fn starting_and_stopping_go_to_capture_and_its_refusals_come_back_as_conflicts() {
    let folder = scratch("control");
    let _capture = FakeCapture::start(&folder.join("capture.sock"), recording);
    let json_body = |text| Some(("application/json", text));

    let (status, body) = call(
        state(&folder),
        "POST",
        "/api/episodes/start",
        json_body(r#"{"duration_s":5}"#),
    )
    .await;
    assert_eq!(
        (status, body),
        (
            StatusCode::CREATED,
            json!({"episode": "20260310T120000.000Z-1a2b3c4d"})
        )
    );

    let (status, body) = call(
        state(&folder),
        "POST",
        "/api/episodes/start",
        json_body("{}"),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["code"], "capture.already_recording");

    for uri in ["/api/episodes/stop", "/api/episodes/abort"] {
        let (status, body) = call(state(&folder), "POST", uri, json_body("{}")).await;
        assert_eq!(
            (status, body),
            (
                StatusCode::OK,
                json!({"episode": "20260310T120000.000Z-1a2b3c4d"})
            ),
            "{uri}"
        );
    }
}

/// A capture process between episodes whose pre-flight found `arm_joints`
/// silent: it starts only when that check is waived.
fn arm_silent(request: &Request) -> Response {
    match request {
        Request::Status => Response::Status(Status {
            preflight: Some(silent_arm(false)),
            ..Status::default()
        }),
        Request::Preflight => Response::Preflight(silent_arm(false)),
        Request::Start { waive, .. } if waive == &["preflight.stream_silent"] => {
            Response::Started {
                episode: "20260310T120000.000Z-1a2b3c4d".into(),
            }
        }
        Request::Start { .. } => Response::error(
            &KaptoError::new(ErrorCode::CapturePreflightFailed)
                .detail("the run at 2026-03-15T20:00:00.000Z failed preflight.stream_silent (arm_joints); not waived"),
        ),
        other => idle(other),
    }
}

#[tokio::test]
async fn pre_flight_runs_on_request_and_a_start_it_refuses_succeeds_once_what_failed_is_waived() {
    let folder = scratch("preflight");
    let _capture = FakeCapture::start(&folder.join("capture.sock"), arm_silent);
    let post = |uri: &'static str, body: &'static str| {
        let state = state(&folder);
        async move { call(state, "POST", uri, Some(("application/json", body))).await }
    };

    let (status, run) = post("/api/preflight", "{}").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(run["passed"], false);
    let failed = &run["checks"][1];
    assert_eq!(
        (&failed["code"], &failed["stream"], &failed["waived"]),
        (
            &json!("preflight.stream_silent"),
            &json!("arm_joints"),
            &json!(false)
        )
    );
    assert_eq!(failed["remedy"], ErrorCode::PreflightStreamSilent.remedy());
    let (_, status_body) = call(state(&folder), "GET", "/api/status", None).await;
    assert_eq!(status_body["capture"]["preflight"], run, "the latest run");

    let (status, body) = post("/api/episodes/start", "{}").await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["code"], "capture.preflight_failed");
    assert!(
        body["message"]
            .as_str()
            .unwrap()
            .contains("preflight.stream_silent (arm_joints)"),
        "{body}"
    );
    let (status, body) = post(
        "/api/episodes/start",
        r#"{"waive":["preflight.stream_silent"]}"#,
    )
    .await;
    assert_eq!(
        (status, body),
        (
            StatusCode::CREATED,
            json!({"episode": "20260310T120000.000Z-1a2b3c4d"})
        )
    );
}

#[tokio::test]
async fn pre_flight_is_refused_while_an_episode_is_recorded() {
    let folder = scratch("preflight-recording");
    let _capture = FakeCapture::start(&folder.join("capture.sock"), recording);
    let (status, body) = call(
        state(&folder),
        "POST",
        "/api/preflight",
        Some(("application/json", "{}")),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["code"], "capture.already_recording");
}

#[tokio::test]
async fn aborting_while_nothing_is_recorded_is_a_conflict_with_its_cause() {
    let folder = scratch("abort-idle");
    let _capture = FakeCapture::start(&folder.join("capture.sock"), idle);
    let (status, body) = call(
        state(&folder),
        "POST",
        "/api/episodes/abort",
        Some(("application/json", "{}")),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["code"], "capture.not_recording");
}

/// Takes every event asked for, from the API only, as recorded in an
/// episode at a fixed moment unless given one.
fn takes_events(request: &Request) -> Response {
    match request {
        Request::Event(event) if event.source == "api" || event.source == "page" => {
            Response::Noted {
                event: Box::new((**event).clone().at(1_772_700_400_000_000_000)),
                episode: Some("20260310T120000.000Z-1a2b3c4d".to_owned()),
            }
        }
        other => recording(other),
    }
}

#[tokio::test]
async fn an_event_of_a_known_kind_goes_to_capture_from_the_api_and_comes_back_as_kept() {
    let folder = scratch("send-event");
    let _capture = FakeCapture::start(&folder.join("capture.sock"), takes_events);
    let send = |body: &'static str| {
        let state = state(&folder);
        async move {
            call(
                state,
                "POST",
                "/api/episodes/events",
                Some(("application/json", body)),
            )
            .await
        }
    };
    let (status, body) =
        send(r#"{"kind":"intervention","phase":"begin","id":"i1","text":"slipped"}"#).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert_eq!(
        body,
        json!({
            "event": {
                "kind": "intervention",
                "at_utc_ns": 1_772_700_400_000_000_000_i64,
                "until_utc_ns": null,
                "phase": "begin",
                "id": "i1",
                "source": "api",
                "text": "slipped",
                "attributes": {}
            },
            "episode": "20260310T120000.000Z-1a2b3c4d"
        })
    );
    let (status, body) = send(r#"{"kind":"note","source":"page"}"#).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert_eq!(body["event"]["source"], "page");
}

#[tokio::test]
async fn an_event_of_an_unknown_kind_or_malformed_is_refused_before_capture_is_asked() {
    let folder = scratch("send-event-refused");
    saved(&folder, "events.toml", "[kinds.grasp]\nstretch = true\n");
    saved(
        &folder,
        "tasks/pick.toml",
        "name = \"Pick\"\nsubtasks = [\"Reach\", \"Lift\"]\n",
    );
    // No capture process: asking it would be 503.
    for (body, status, code) in [
        (
            r#"{"kind":"note"}"#,
            StatusCode::CONFLICT,
            "events.unknown_kind",
        ),
        (
            r#"{"kind":"kapto.stream_lost"}"#,
            StatusCode::BAD_REQUEST,
            "capture.event_invalid",
        ),
        (
            r#"{"kind":"grasp","phase":"begin"}"#,
            StatusCode::BAD_REQUEST,
            "capture.event_invalid",
        ),
        (
            r#"{"kind":"grasp","source":"kapto"}"#,
            StatusCode::BAD_REQUEST,
            "api.invalid_request",
        ),
        (
            r#"{"kind":"grasp","source":"robot:pedal"}"#,
            StatusCode::BAD_REQUEST,
            "api.invalid_request",
        ),
        (
            r#"{"kind":"grasp","phase":"sometimes"}"#,
            StatusCode::BAD_REQUEST,
            "api.invalid_request",
        ),
    ] {
        let (got, reply) = call(
            state(&folder),
            "POST",
            "/api/episodes/events",
            Some(("application/json", body)),
        )
        .await;
        assert_eq!(
            (got, reply["code"].as_str().unwrap()),
            (status, code),
            "{body}"
        );
    }
    let (_, reply) = call(
        state(&folder),
        "POST",
        "/api/episodes/events",
        Some(("application/json", r#"{"kind":"note"}"#)),
    )
    .await;
    let message = reply["message"].as_str().unwrap();
    assert!(
        message.contains("the kinds are grasp, subtask.reach, subtask.lift"),
        "{message}"
    );
    // Known kinds get as far as capture.
    for body in [
        r#"{"kind":"grasp"}"#,
        r#"{"kind":"subtask.lift","phase":"end","id":"l1"}"#,
    ] {
        let (got, reply) = call(
            state(&folder),
            "POST",
            "/api/episodes/events",
            Some(("application/json", body)),
        )
        .await;
        assert_eq!(got, StatusCode::SERVICE_UNAVAILABLE, "{body} {reply}");
    }
}
