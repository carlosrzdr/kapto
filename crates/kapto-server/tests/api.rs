// SPDX-License-Identifier: Apache-2.0

//! The API's handlers, called through its router, with a stand-in for the
//! capture process on a real control socket.

#![cfg(unix)]
#![allow(
    clippy::unwrap_used,
    clippy::panic,
    reason = "test helpers fail the test loudly when the setup itself breaks"
)]

use std::net::IpAddr;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use axum::body::Body;
use axum::http::{Request as HttpRequest, StatusCode, header};
use kapto_capture::control::{
    ControlServer, LiveViewCounts, Request, Response, Status, StreamStatus,
};
use kapto_capture::disk::DiskLimits;
use kapto_capture::finalise::Staged;
use kapto_core::{EpisodeId, ErrorCode, KaptoError};
use kapto_server::api::{ApiError, ApiState, CaptureAnnouncements, openapi_json, router};
use kapto_server::index;
use kapto_server::jobs::REPLAY;
use kapto_server::replays::view_file;
use kapto_server::robot::RobotOverview;
use kapto_server::scene::{MESH_MAGIC, RobotScene, Scene};
use kapto_server::settings::{Setting, SettingsOverview};
use kapto_server::{Event, Processes};
use kapto_store::replay_view::{VIEW_MAGIC, VIEW_VERSION};
use kapto_store::{
    CheckKind, Deletion, DeletionReason, Episode, JobFailure, JobState, OperationMode,
    PackageIdentity, Preflight, PreflightCheck, QaResult, QaRun, Recovered, RemoteCopy,
    SessionProfile, SessionTask, TimeRange, UploadCounts, UploadedObject, Verdict,
};
use kapto_testkit::scratch;
use kapto_viz::stream::{OpeningStream, OpeningValue};
use kapto_viz::view::{ViewContents, ViewWriter};
use serde_json::{Value, json};
use tower::ServiceExt;

/// The API's state for a data directory `folder/data`, its episode index
/// rebuilt from what the folder holds now.
fn state(folder: &Path) -> ApiState {
    ApiState {
        processes: Arc::new(Processes::new("serve", &["capture", "viz-bridge"])),
        robot: Arc::new(RobotOverview::default()),
        scene: Arc::new(Scene::default()),
        settings: Arc::new(SettingsOverview::default()),
        config_folder: folder.join("config"),
        package: Some("arm7".to_owned()),
        bind: IpAddr::from([127, 0, 0, 1]),
        control_socket: folder.join("capture.sock"),
        data_dir: folder.join("data"),
        store: index::open(&folder.join("data")).unwrap(),
        viewer_stream_port: 9877,
        announcements: CaptureAnnouncements::default(),
        disk_limits: DiskLimits {
            warn_below: 10 << 30,
            stop_below: 2 << 30,
        },
        destinations: Vec::new(),
    }
}

/// A capture process that answers every request with `answer`.
struct FakeCapture {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl FakeCapture {
    fn start(socket: &Path, answer: fn(&Request) -> Response) -> Self {
        let server = ControlServer::listen(socket).unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let thread = thread::spawn({
            let stop = Arc::clone(&stop);
            move || {
                while !stop.load(Ordering::Relaxed) {
                    for pending in server.pending() {
                        let response = answer(&pending.request);
                        pending.answer(response);
                    }
                    thread::sleep(Duration::from_millis(5));
                }
            }
        });
        Self {
            stop,
            thread: Some(thread),
        }
    }
}

impl Drop for FakeCapture {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            thread.join().unwrap();
        }
    }
}

/// Sends one request through the router and returns the status and JSON.
async fn call(
    state: ApiState,
    method: &str,
    uri: &str,
    body: Option<(&str, &str)>,
) -> (StatusCode, Value) {
    let mut request = HttpRequest::builder().method(method).uri(uri);
    let body = match body {
        Some((content_type, text)) => {
            request = request.header(header::CONTENT_TYPE, content_type);
            Body::from(text.to_owned())
        }
        None => Body::empty(),
    };
    let response = router(state)
        .oneshot(request.body(body).unwrap())
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

/// A pre-flight run in which the disk had room and `arm_joints` sent
/// nothing, its failure `waived` or not.
fn silent_arm(waived: bool) -> Preflight {
    let check = |code: &str, stream: Option<&str>, passed| PreflightCheck {
        code: code.to_owned(),
        stream: stream.map(str::to_owned),
        passed,
        measured: "measured".to_owned(),
        needed: "needed".to_owned(),
        waived: waived && !passed,
    };
    Preflight {
        ran_at: "2026-03-15T20:00:00.000Z".to_owned(),
        window_ms: 5000,
        checks: vec![
            check("preflight.disk_too_small", None, true),
            check("preflight.stream_silent", Some("arm_joints"), false),
        ],
    }
}

fn recording(request: &Request) -> Response {
    match request {
        Request::Status => Response::Status(Status {
            episode: Some("20260310T120000.000Z-1a2b3c4d".into()),
            seconds: 7,
            streams: vec![
                StreamStatus {
                    stream: "arm_joints".into(),
                    received: 14_000,
                    written: 13_990,
                    dropped_by_us: 10,
                    lost_upstream: 2,
                    values: Some(vec![0.5, -1.25, 0.04]),
                    values_age_ms: Some(12),
                    live_view: None,
                },
                StreamStatus {
                    stream: "wrist_camera".into(),
                    received: 210,
                    written: 210,
                    live_view: Some(LiveViewCounts {
                        offered: 200,
                        skipped: 1,
                        superseded: 30,
                        not_sent: 4,
                        sent: 165,
                    }),
                    ..StreamStatus::default()
                },
            ],
            preflight: Some(silent_arm(true)),
        }),
        Request::Start {
            duration_s: Some(5),
            ..
        } => Response::Started {
            episode: "20260310T120000.000Z-1a2b3c4d".into(),
        },
        Request::Start { .. } => {
            Response::error(&KaptoError::new(ErrorCode::CaptureAlreadyRecording))
        }
        Request::Stop => Response::Stopped {
            episode: "20260310T120000.000Z-1a2b3c4d".into(),
            folder: "/data/20260310T120000.000Z-1a2b3c4d".into(),
        },
        Request::Abort => Response::Aborted {
            episode: "20260310T120000.000Z-1a2b3c4d".into(),
        },
        Request::Preflight => Response::error(&KaptoError::new(ErrorCode::CaptureAlreadyRecording)),
        Request::Event(_) => {
            Response::error(&KaptoError::new(ErrorCode::CaptureControlInvalidRequest))
        }
        Request::Hello { .. } | Request::Supervise | Request::Watch => {
            unreachable!("answered by the connection")
        }
    }
}

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
async fn the_robot_is_what_serve_loaded_even_while_capture_is_down() {
    let folder = scratch("robot");
    let state = ApiState {
        robot: Arc::new(RobotOverview {
            name: "arm7".into(),
            links: 11,
            warnings: vec![ApiError::from(&KaptoError::new(
                ErrorCode::MappingJointUnmapped,
            ))],
            ..RobotOverview::default()
        }),
        ..state(&folder)
    };
    let (status, body) = call(state, "GET", "/api/robot", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["name"], "arm7");
    assert_eq!(body["links"], 11);
    assert_eq!(body["warnings"][0]["code"], "mapping.joint_unmapped");
}

/// Writes `contents` to `path` inside the test's configuration folder.
fn saved(folder: &Path, path: &str, contents: &str) {
    let path = folder.join("config").join(path);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, contents).unwrap();
}

#[tokio::test]
async fn profiles_are_listed_each_available_only_for_the_robot_package_kapto_runs() {
    let folder = scratch("profiles");
    saved(
        &folder,
        "profiles/lab_ana.toml",
        "package = \"arm7\"\noperator = \"Ana\"\nmode = \"teleoperated\"\n",
    );
    saved(&folder, "profiles/cobot_day.toml", "package = \"cobot\"\n");
    saved(
        &folder,
        "profiles/broken.toml",
        "package = \"arm7\"\noperator = \"na\"\n",
    );
    let (status, body) = call(state(&folder), "GET", "/api/profiles", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["package"], "arm7");
    let listed: Vec<(&str, bool)> = body["profiles"]
        .as_array()
        .unwrap()
        .iter()
        .map(|profile| {
            (
                profile["id"].as_str().unwrap(),
                profile["available"].as_bool().unwrap(),
            )
        })
        .collect();
    assert_eq!(listed, [("cobot_day", false), ("lab_ana", true)]);
    assert_eq!(body["profiles"][1]["mode"], "teleoperated");
    assert_eq!(body["problems"].as_array().unwrap().len(), 1);
    assert_eq!(body["problems"][0]["code"], "config.invalid_value");

    // Saved while serve runs: listed at the next request.
    saved(&folder, "profiles/lab_ben.toml", "package = \"arm7\"\n");
    let (_, body) = call(state(&folder), "GET", "/api/profiles", None).await;
    assert_eq!(body["profiles"].as_array().unwrap().len(), 3);
}

#[tokio::test]
async fn tasks_are_listed_with_what_is_wrong_with_the_others() {
    let folder = scratch("tasks");
    let (_, body) = call(state(&folder), "GET", "/api/tasks", None).await;
    assert_eq!(body, json!({"tasks": [], "problems": []}));
    saved(
        &folder,
        "tasks/pick_and_place.toml",
        "name = \"Pick and place\"\nsubtasks = [\"reach\", \"grasp\"]\n",
    );
    saved(&folder, "tasks/broken.toml", "steps = [\"reach\"]\n");
    let (status, body) = call(state(&folder), "GET", "/api/tasks", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["tasks"][0]["id"], "pick_and_place");
    assert_eq!(body["tasks"][0]["subtasks"], json!(["reach", "grasp"]));
    assert_eq!(body["problems"][0]["code"], "task.unknown_key");
}

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
async fn the_settings_are_what_serve_started_with_with_their_sources() {
    let folder = scratch("settings");
    let state = ApiState {
        settings: Arc::new(SettingsOverview {
            file: "/config/kapto.toml".into(),
            file_found: true,
            settings: vec![Setting {
                key: "server.port".into(),
                value: "7701".into(),
                source: "/config/kapto.toml:2".into(),
                default: false,
                help: "Port of the API and web UI.".into(),
            }],
        }),
        ..state(&folder)
    };
    let (status, body) = call(state, "GET", "/api/settings", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["file_found"], true);
    assert_eq!(body["settings"][0]["key"], "server.port");
    assert_eq!(body["settings"][0]["source"], "/config/kapto.toml:2");
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

fn idle(request: &Request) -> Response {
    match request {
        Request::Stop | Request::Abort => {
            Response::error(&KaptoError::new(ErrorCode::CaptureNotRecording))
        }
        other => recording(other),
    }
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

#[tokio::test]
async fn the_event_kinds_are_the_vocabulary_files_or_the_defaults_then_each_tasks_subtasks() {
    let folder = scratch("event-kinds");
    let kinds = |body: &Value| -> Vec<String> {
        body["kinds"]
            .as_array()
            .unwrap()
            .iter()
            .map(|kind| {
                format!(
                    "{} {}",
                    kind["kind"].as_str().unwrap(),
                    kind["from"].as_str().unwrap()
                )
            })
            .collect()
    };
    let (status, body) = call(state(&folder), "GET", "/api/event-kinds", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        kinds(&body),
        ["note default", "problem default", "intervention default"]
    );
    saved(
        &folder,
        "events.toml",
        "[kinds.grasp]\nname = \"Grasp\"\ncolor = \"#4c9be8\"\nstretch = true\n",
    );
    saved(&folder, "tasks/pick.toml", "subtasks = [\"Reach\"]\n");
    let (_, body) = call(state(&folder), "GET", "/api/event-kinds", None).await;
    assert_eq!(kinds(&body), ["grasp file", "subtask.reach task"]);
    assert_eq!(
        body["kinds"][0],
        json!({"kind": "grasp", "name": "Grasp", "color": "#4c9be8", "stretch": true, "description": null, "from": "file", "task": null})
    );
    assert_eq!(body["kinds"][1]["task"], "pick");
    // A file with problems: the defaults stand in, and its problems say why.
    saved(&folder, "events.toml", "[kinds.grasp]\nshade = \"blue\"\n");
    let (_, body) = call(state(&folder), "GET", "/api/event-kinds", None).await;
    assert_eq!(kinds(&body)[0], "note default");
    assert_eq!(body["problems"][0]["code"], "events.unknown_key");
}

/// A finalised episode in `data` that started at `second` past a fixed time.
fn finalised_episode(data: &Path, id: &str, second: i64) {
    finalised_episode_with(data, id, second, None);
}

/// The same, recorded with `package`, or `unchecked` without.
fn finalised_episode_with(data: &Path, id: &str, second: i64, package: Option<PackageIdentity>) {
    let id: EpisodeId = id.parse().unwrap();
    let staged = Staged::create(data, &id).unwrap();
    std::fs::write(staged.recording_path(), b"not really mcap").unwrap();
    let start = 1_772_700_400_000_000_000 + second * 1_000_000_000;
    kapto_testkit::episode(
        &id.to_string(),
        (start, start + 1_000_000_000),
        vec![kapto_testkit::stream("arm_joints", 2000)],
    )
    .with_package(package)
    .write(&staged.dir())
    .unwrap();
    staged.finalise().unwrap();
}

#[tokio::test]
async fn each_episode_names_its_robot_package_or_null_if_unchecked() {
    let folder = scratch("episode-package");
    let data = folder.join("data");
    let checked = "20260310T120000.000Z-00000001";
    let package = PackageIdentity {
        id: "arm7".into(),
        version: "1.0.0".into(),
        hash: "9f2c".into(),
    };
    finalised_episode_with(&data, checked, 0, Some(package));
    finalised_episode(&data, "20260310T120100.000Z-00000002", 60);

    let (_, list) = call(state(&folder), "GET", "/api/episodes", None).await;
    assert_eq!(list[0]["package"], Value::Null, "the newest is unchecked");
    assert_eq!(
        list[1]["package"],
        json!({"id": "arm7", "version": "1.0.0", "hash": "9f2c"})
    );
    let (_, detail) = call(
        state(&folder),
        "GET",
        &format!("/api/episodes/{checked}"),
        None,
    )
    .await;
    assert_eq!(detail["package"]["id"], "arm7");
}

#[tokio::test]
async fn an_episode_names_its_profile_and_task_in_the_list_and_shows_them_in_full() {
    let folder = scratch("episode-session");
    let data = folder.join("data");
    let id = "20260310T120000.000Z-00000001";
    finalised_episode(&data, id, 0);
    finalised_episode(&data, "20260310T120500.000Z-00000002", 300);
    // Recorded with a profile and a task.
    let episode_folder = data.join(id);
    let recorded = Episode::read(&episode_folder).unwrap().with_session(
        Some(SessionProfile {
            id: "lab_ana".into(),
            hash: None,
            operator: Some("Ana".into()),
            environment: None,
            mode: Some(OperationMode::Teleoperated),
            tags: vec!["shift-a".into()],
            notes: None,
        }),
        Some(SessionTask {
            id: "pick_and_place".into(),
            hash: None,
            name: Some("Pick and place".into()),
            description: None,
            prompt: Some("Pick up the red cube".into()),
            objects: Vec::new(),
            subtasks: vec!["reach".into(), "grasp".into()],
        }),
    );
    recorded.write(&episode_folder).unwrap();

    let (_, list) = call(state(&folder), "GET", "/api/episodes", None).await;
    let session = |episode: &Value| {
        ["profile", "mode", "task", "task_name"].map(|field| episode[field].clone())
    };
    assert_eq!(
        session(&list[0]),
        [Value::Null, Value::Null, Value::Null, Value::Null]
    );
    assert_eq!(
        session(&list[1]),
        [
            json!("lab_ana"),
            json!("teleoperated"),
            json!("pick_and_place"),
            json!("Pick and place")
        ]
    );
    let (_, detail) = call(state(&folder), "GET", &format!("/api/episodes/{id}"), None).await;
    assert_eq!(detail["profile"]["operator"], "Ana");
    assert_eq!(detail["profile"]["tags"], json!(["shift-a"]));
    assert_eq!(detail["task"]["prompt"], "Pick up the red cube");
    assert_eq!(detail["task"]["subtasks"], json!(["reach", "grasp"]));
}

#[tokio::test]
async fn a_recovered_episode_says_so_in_the_list_and_in_full_and_others_say_null() {
    let folder = scratch("episode-recovered");
    let data = folder.join("data");
    let id = "20260310T120000.000Z-00000001";
    finalised_episode(&data, id, 0);
    finalised_episode(&data, "20260310T120500.000Z-00000002", 300);
    let episode_folder = data.join(id);
    Episode::read(&episode_folder)
        .unwrap()
        .with_recovered(Recovered {
            at_utc_ns: 1_772_701_000_000_000_000,
            bytes_cut: 4096,
            last_message_utc_ns: Some(1_772_700_400_900_000_000),
            reason: "recording stopped without finishing it".into(),
        })
        .write(&episode_folder)
        .unwrap();

    let recovered = json!({
        "at_utc_ns": 1_772_701_000_000_000_000_i64,
        "bytes_cut": 4096,
        "last_message_utc_ns": 1_772_700_400_900_000_000_i64,
        "reason": "recording stopped without finishing it",
    });
    let (_, list) = call(state(&folder), "GET", "/api/episodes", None).await;
    assert_eq!(list[0]["recovered"], Value::Null);
    assert_eq!(list[1]["recovered"], recovered);
    let (_, detail) = call(state(&folder), "GET", &format!("/api/episodes/{id}"), None).await;
    assert_eq!(detail["recovered"], recovered);
}

#[tokio::test]
async fn episodes_lists_the_finalised_ones_newest_first_and_nothing_else() {
    let folder = scratch("episodes");
    let data = folder.join("data");
    finalised_episode(&data, "20260310T120000.000Z-00000001", 0);
    finalised_episode(&data, "20260310T120500.000Z-00000002", 300);
    std::fs::create_dir_all(data.join("20260310T121000.000Z-00000003")).unwrap();
    std::fs::create_dir_all(data.join("notes")).unwrap();

    let (status, body) = call(state(&folder), "GET", "/api/episodes", None).await;
    assert_eq!(status, StatusCode::OK);
    let ids: Vec<&str> = body
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["id"].as_str().unwrap())
        .collect();
    assert_eq!(
        ids,
        [
            "20260310T120500.000Z-00000002",
            "20260310T120000.000Z-00000001"
        ]
    );
    assert_eq!(body[0]["streams"][0]["written"], 2000);

    let (_, body) = call(state(&folder), "GET", "/api/episodes?limit=1", None).await;
    assert_eq!(body.as_array().unwrap().len(), 1);
}

/// The IDs `/api/episodes` lists.
async fn listed(state: ApiState) -> Vec<String> {
    let (_, body) = call(state, "GET", "/api/episodes", None).await;
    body.as_array()
        .unwrap()
        .iter()
        .map(|episode| episode["id"].as_str().unwrap().to_owned())
        .collect()
}

#[tokio::test]
async fn an_episode_capture_announces_finalised_is_listed_at_once_and_one_added_meanwhile_after_a_restart()
 {
    let folder = scratch("indexed");
    let data = folder.join("data");
    let serving = state(&folder);
    let running = Arc::new(AtomicBool::new(true));
    let indexing = thread::spawn({
        let (store, data, running) = (serving.store.clone(), data.clone(), Arc::clone(&running));
        let announced = serving.announcements.subscribe();
        move || index::keep_up_to_date(&store, &data, announced, &running)
    });

    let id = "20260310T120000.000Z-00000001";
    finalised_episode(&data, id, 0);
    serving
        .announcements
        .announce(kapto_capture::control::Announcement::EpisodeEnded {
            episode: id.into(),
            finalised: true,
        });
    let mut tries = 0;
    while listed(serving.clone()).await.is_empty() && tries < 100 {
        tokio::time::sleep(Duration::from_millis(20)).await;
        tries += 1;
    }
    assert_eq!(listed(serving.clone()).await, [id]);

    // Copied in without capture announcing it: listed once serve starts
    // again and rebuilds the index.
    let copied = "20260310T120500.000Z-00000002";
    finalised_episode(&data, copied, 300);
    assert_eq!(listed(serving.clone()).await, [id]);
    let restarted = state(&folder);
    assert_eq!(listed(restarted.clone()).await, [copied, id]);
    // Its events are queued to be read; those of the one announced already
    // were, and aren't queued twice.
    let events: Vec<String> = restarted
        .store
        .reader()
        .unwrap()
        .jobs(10)
        .unwrap()
        .into_iter()
        .filter(|job| job.kind == "events")
        .map(|job| job.episode)
        .collect();
    assert_eq!(events, [copied, id]);

    running.store(false, Ordering::Relaxed);
    indexing.join().unwrap();
}

#[tokio::test]
async fn an_episode_capture_announces_recovered_is_listed_and_queued_to_be_checked() {
    let folder = scratch("recovered-listed");
    let data = folder.join("data");
    let serving = state(&folder);
    let running = Arc::new(AtomicBool::new(true));
    let indexing = thread::spawn({
        let (store, data, running) = (serving.store.clone(), data.clone(), Arc::clone(&running));
        let announced = serving.announcements.subscribe();
        move || index::keep_up_to_date(&store, &data, announced, &running)
    });

    let id = "20260310T120000.000Z-00000001";
    finalised_episode(&data, id, 0);
    serving
        .announcements
        .announce(kapto_capture::control::Announcement::EpisodeRecovered { episode: id.into() });
    let mut tries = 0;
    while listed(serving.clone()).await.is_empty() && tries < 100 {
        tokio::time::sleep(Duration::from_millis(20)).await;
        tries += 1;
    }
    assert_eq!(listed(serving.clone()).await, [id]);
    let jobs = serving.store.reader().unwrap().jobs(10).unwrap();
    assert_eq!(
        jobs.iter()
            .map(|job| (job.kind.as_str(), job.episode.as_str()))
            .collect::<Vec<_>>(),
        [("qa", id), ("events", id), ("verify", id)]
    );

    running.store(false, Ordering::Relaxed);
    indexing.join().unwrap();
}

#[tokio::test]
async fn an_episode_stopped_through_the_api_is_listed_as_soon_as_stop_answers() {
    let folder = scratch("stopped-listed");
    let _capture = FakeCapture::start(&folder.join("capture.sock"), recording);
    let serving = state(&folder);
    // What capture finalised just before answering.
    let id = "20260310T120000.000Z-1a2b3c4d";
    finalised_episode(&serving.data_dir, id, 0);
    let body = Some(("application/json", "{}"));
    let (status, _) = call(serving.clone(), "POST", "/api/episodes/stop", body).await;
    assert_eq!(status, StatusCode::OK);
    // Its files are queued to be checked, then its events read and its
    // quality checked: listed newest first.
    let jobs = serving.store.reader().unwrap().jobs(10).unwrap();
    assert_eq!(
        jobs.iter()
            .map(|job| (job.kind.as_str(), job.episode.as_str()))
            .collect::<Vec<_>>(),
        [("qa", id), ("events", id), ("verify", id)]
    );
    assert_eq!(listed(serving).await, [id]);
}

#[tokio::test]
async fn an_unreadable_episode_index_is_an_error_with_its_cause() {
    let folder = scratch("no-index");
    let state = state(&folder);
    std::fs::remove_file(folder.join("data/kapto.db")).unwrap();
    let (status, body) = call(state, "GET", "/api/episodes", None).await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(body["code"], "store.open_failed");
}

#[tokio::test]
async fn an_episode_in_full_has_its_streams_files_buffers_and_durability() {
    let folder = scratch("episode");
    let data = folder.join("data");
    let id = "20260310T120000.000Z-00000001";
    finalised_episode(&data, id, 0);

    let (status, body) = call(state(&folder), "GET", &format!("/api/episodes/{id}"), None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["id"], id);
    assert_eq!(body["folder"], data.join(id).display().to_string());
    assert_eq!(
        body["ended_utc_ns"].as_i64().unwrap() - body["started_utc_ns"].as_i64().unwrap(),
        1_000_000_000
    );
    let stream = &body["streams"][0];
    assert_eq!(stream["stream"], "arm_joints");
    assert_eq!(stream["loss_detection"], "sequence");
    assert_eq!(stream["written"], 2000);
    assert_eq!(stream["disconnects"], Value::Null);
    let files: Vec<&str> = body["files"]
        .as_array()
        .unwrap()
        .iter()
        .map(|file| file["path"].as_str().unwrap())
        .collect();
    assert!(
        files.contains(&"recording.mcap") && files.contains(&"episode.json"),
        "{files:?}"
    );
    assert_eq!(body["buffers"]["peak_batches"], 1);
    assert_eq!(body["durability"]["syncs"], 2);
    assert_eq!(body["clock"]["steps"], 0);
}

#[tokio::test]
async fn an_episode_that_isnt_there_or_isnt_an_id_is_refused_with_its_cause() {
    let folder = scratch("episode-missing");
    let data = folder.join("data");
    // Being recorded, so not finalised.
    std::fs::create_dir_all(data.join("20260310T121000.000Z-00000003")).unwrap();
    for id in [
        "20260310T121000.000Z-00000003",
        "20260310T120000.000Z-0000000f",
    ] {
        let (status, body) =
            call(state(&folder), "GET", &format!("/api/episodes/{id}"), None).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{id}");
        assert_eq!(body["code"], "api.episode_not_found");
        assert_eq!(body["remedy"], ErrorCode::ApiEpisodeNotFound.remedy());
    }
    // Never a path to anywhere else.
    let (status, body) = call(state(&folder), "GET", "/api/episodes/..%2F..%2Fetc", None).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["code"], "api.invalid_request");
}

/// A run of `streams.complete` on episode `id`, as the worker reports it.
fn checked(id: &str, verdict: Verdict) -> QaRun {
    QaRun {
        statistics: std::collections::BTreeMap::new(),
        episode: id.to_owned(),
        params_version: Some("7".to_owned()),
        computed_utc_ns: 1_772_700_500_000_000_000,
        results: vec![QaResult {
            check: "streams.complete".to_owned(),
            version: 1,
            kind: CheckKind::Gate,
            verdict,
            message: "Kapto dropped 12 messages: arm_joints (12 of 2000 received)".to_owned(),
            values: std::collections::BTreeMap::from([("dropped_by_us".to_owned(), 12.0)]),
            ranges: vec![TimeRange {
                start_utc_ns: 1_772_700_410_000_000_000,
                end_utc_ns: 1_772_700_411_000_000_000,
            }],
        }],
        model: None,
    }
}

#[tokio::test]
async fn an_episodes_quality_results_are_shown_and_episodes_are_listed_and_filtered_by_them() {
    let folder = scratch("quality");
    let data = folder.join("data");
    let (accepted, rejected, unchecked) = (
        "20260310T120000.000Z-00000001",
        "20260310T120500.000Z-00000002",
        "20260310T121000.000Z-00000003",
    );
    for (n, id) in [accepted, rejected, unchecked].into_iter().enumerate() {
        finalised_episode(&data, id, i64::try_from(n).unwrap() * 300);
    }
    let serving = state(&folder);
    serving
        .store
        .record_qa(checked(accepted, Verdict::Passed))
        .unwrap();
    serving
        .store
        .record_qa(checked(rejected, Verdict::Failed))
        .unwrap();

    let (status, body) = call(
        serving.clone(),
        "GET",
        &format!("/api/episodes/{rejected}/qa"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        body,
        json!({
            "accepted": false,
            "params_version": "7",
            "computed_utc_ns": 1_772_700_500_000_000_000_i64,
            "checks": [{
                "check": "streams.complete",
                "version": 1,
                "kind": "gate",
                "verdict": "failed",
                "message": "Kapto dropped 12 messages: arm_joints (12 of 2000 received)",
                "values": {"dropped_by_us": 12.0},
                "ranges": [{
                    "start_utc_ns": 1_772_700_410_000_000_000_i64,
                    "end_utc_ns": 1_772_700_411_000_000_000_i64,
                }],
            }],
        })
    );
    // Not checked yet: nothing decided, nothing found.
    let (status, body) = call(
        serving.clone(),
        "GET",
        &format!("/api/episodes/{unchecked}/qa"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        body,
        json!({"accepted": null, "params_version": null, "computed_utc_ns": null, "checks": []})
    );

    let listed = |query: &'static str| {
        let serving = serving.clone();
        async move {
            let (_, body) = call(serving, "GET", query, None).await;
            body.as_array()
                .unwrap()
                .iter()
                .map(|episode| {
                    (
                        episode["id"].as_str().unwrap().to_owned(),
                        episode["accepted"].clone(),
                    )
                })
                .collect::<Vec<_>>()
        }
    };
    assert_eq!(
        listed("/api/episodes").await,
        [
            (unchecked.to_owned(), Value::Null),
            (rejected.to_owned(), json!(false)),
            (accepted.to_owned(), json!(true)),
        ]
    );
    assert_eq!(
        listed("/api/episodes?accepted=false").await,
        [(rejected.to_owned(), json!(false))]
    );
    assert_eq!(
        listed("/api/episodes?accepted=true").await,
        [(accepted.to_owned(), json!(true))]
    );
}

#[tokio::test]
async fn an_episodes_statistics_are_summarised_and_none_until_it_is_checked() {
    use kapto_qa::episode_statistics::Statistic;
    use kapto_qa::statistics::Distribution;

    let folder = scratch("statistics");
    let data = folder.join("data");
    let (checked_id, unchecked) = (
        "20260310T120000.000Z-00000001",
        "20260310T120500.000Z-00000002",
    );
    finalised_episode(&data, checked_id, 0);
    finalised_episode(&data, unchecked, 300);
    let serving = state(&folder);
    let mut run = checked(checked_id, Verdict::Passed);
    let speeds = Distribution::of((1..=100).map(f64::from));
    run.statistics = std::collections::BTreeMap::from([
        (
            "episode.duration_s".to_owned(),
            serde_json::to_value(Statistic::Value(10.0)).unwrap(),
        ),
        (
            "joints.joint_1.speed".to_owned(),
            serde_json::to_value(Statistic::Distribution(speeds)).unwrap(),
        ),
        // A form this version doesn't read: left out.
        ("from.the_future".to_owned(), json!({"sketch": [1, 2]})),
    ]);
    serving.store.record_qa(run).unwrap();

    let (status, body) = call(
        serving.clone(),
        "GET",
        &format!("/api/episodes/{checked_id}/statistics"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["computed_utc_ns"], 1_772_700_500_000_000_000_i64);
    let statistics = body["statistics"].as_array().unwrap();
    assert_eq!(statistics.len(), 2);
    assert_eq!(statistics[0]["name"], "episode.duration_s");
    assert_eq!(statistics[0]["value"], 10.0);
    assert_eq!(statistics[0]["count"], Value::Null);
    let speed = &statistics[1];
    assert_eq!(speed["name"], "joints.joint_1.speed");
    assert_eq!(
        (
            &speed["count"],
            &speed["min"],
            &speed["max"],
            &speed["mean"]
        ),
        (&json!(100), &json!(1.0), &json!(100.0), &json!(50.5))
    );
    for (field, exact) in [("p50", 51.0), ("p90", 90.0), ("p99", 99.0)] {
        let estimate = speed[field].as_f64().unwrap();
        assert!(
            (estimate - exact).abs() <= exact * 0.01,
            "{field}: {estimate}"
        );
    }

    let (status, body) = call(
        serving.clone(),
        "GET",
        &format!("/api/episodes/{unchecked}/statistics"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, json!({"computed_utc_ns": null, "statistics": []}));
    let (status, body) = call(serving, "GET", "/api/episodes/notanid/statistics", None).await;
    assert_eq!(
        (status, &body["code"]),
        (StatusCode::BAD_REQUEST, &json!("api.invalid_request"))
    );
}

#[tokio::test]
async fn the_quality_of_an_episode_that_isnt_there_or_isnt_an_id_is_refused_with_its_cause() {
    let folder = scratch("quality-missing");
    let (status, body) = call(
        state(&folder),
        "GET",
        "/api/episodes/20260310T120000.000Z-0000000f/qa",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["code"], "api.episode_not_found");
    let (status, body) = call(state(&folder), "GET", "/api/episodes/notanid/qa", None).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["code"], "api.invalid_request");
    let (status, _) = call(state(&folder), "GET", "/api/episodes?accepted=maybe", None).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

/// A moment or one end of a stretch of `kind`, sent through the API.
fn marked(
    kind: &str,
    phase: kapto_store::Phase,
    id: Option<&str>,
    at_utc_ns: i64,
) -> kapto_store::Event {
    kapto_store::Event {
        kind: kind.to_owned(),
        at_utc_ns,
        until_utc_ns: None,
        phase,
        id: id.map(str::to_owned),
        source: "api".to_owned(),
        text: None,
        attributes: std::collections::BTreeMap::new(),
    }
}

#[tokio::test]
async fn an_episodes_events_are_shown_with_its_unended_stretches_and_episodes_found_by_kind() {
    use kapto_store::Phase;

    let folder = scratch("episode-events");
    let data = folder.join("data");
    let (marked_one, quiet, unread) = (
        "20260310T120000.000Z-00000001",
        "20260310T120100.000Z-00000002",
        "20260310T120200.000Z-00000003",
    );
    for (n, id) in [marked_one, quiet, unread].into_iter().enumerate() {
        finalised_episode(&data, id, i64::try_from(n).unwrap() * 60);
    }
    let api = state(&folder);
    // Not read yet: said so, and nothing.
    let (status, body) = call(
        api.clone(),
        "GET",
        &format!("/api/episodes/{unread}/events"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, json!({"read": false, "events": [], "unended": []}));

    api.store
        .record_events(
            marked_one,
            vec![
                marked("intervention", Phase::Begin, Some("i1"), 10),
                marked("note", Phase::Moment, None, 20),
                marked("intervention", Phase::End, Some("i1"), 30),
                marked("intervention", Phase::Begin, Some("i2"), 40),
            ],
        )
        .unwrap();
    api.store.record_events(quiet, Vec::new()).unwrap();
    let (_, body) = call(
        api.clone(),
        "GET",
        &format!("/api/episodes/{marked_one}/events"),
        None,
    )
    .await;
    assert_eq!(body["read"], true);
    let kinds: Vec<&str> = body["events"]
        .as_array()
        .unwrap()
        .iter()
        .map(|event| event["kind"].as_str().unwrap())
        .collect();
    assert_eq!(
        kinds,
        ["intervention", "note", "intervention", "intervention"]
    );
    // The second intervention was never ended.
    assert_eq!(body["unended"].as_array().unwrap().len(), 1);
    assert_eq!(body["unended"][0]["id"], "i2");
    assert_eq!(body["unended"][0]["phase"], "begin");

    // Found by kind; each listed with its events by kind, or null if not read.
    let (_, all) = call(api.clone(), "GET", "/api/episodes", None).await;
    let counts: Vec<(&str, &Value)> = all
        .as_array()
        .unwrap()
        .iter()
        .map(|episode| (episode["id"].as_str().unwrap(), &episode["events"]))
        .collect();
    assert_eq!(
        counts,
        [
            (unread, &Value::Null),
            (quiet, &json!({})),
            (marked_one, &json!({"intervention": 3, "note": 1})),
        ]
    );
    let (status, found) = call(api.clone(), "GET", "/api/episodes?event=note", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(found.as_array().unwrap().len(), 1);
    assert_eq!(found[0]["id"], marked_one);
    let (_, none) = call(api.clone(), "GET", "/api/episodes?event=grasp", None).await;
    assert_eq!(none, json!([]));

    // An episode that isn't there, or an ID that isn't one, says why.
    let (status, body) = call(
        api.clone(),
        "GET",
        "/api/episodes/20260310T120000.000Z-0000000f/events",
        None,
    )
    .await;
    assert_eq!(
        (status, &body["code"]),
        (StatusCode::NOT_FOUND, &json!("api.episode_not_found"))
    );
    let (status, body) = call(api, "GET", "/api/episodes/notanid/events", None).await;
    assert_eq!(
        (status, &body["code"]),
        (StatusCode::BAD_REQUEST, &json!("api.invalid_request"))
    );
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

#[test]
fn the_openapi_document_describes_every_route_and_its_error_body() {
    let document: Value = serde_json::from_str(&openapi_json()).unwrap();
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
            "/api/episodes/{id}/events",
            "/api/episodes/{id}/qa",
            "/api/episodes/{id}/replay",
            "/api/episodes/{id}/replay/meshes/{n}",
            "/api/episodes/{id}/replay/opening",
            "/api/episodes/{id}/replay/scene",
            "/api/episodes/{id}/replay/window",
            "/api/episodes/{id}/statistics",
            "/api/episodes/{id}/uploads",
            "/api/episodes/{id}/uploads/retry",
            "/api/event-kinds",
            "/api/events",
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

/// Serves the API on a free loopback port until the test ends.
async fn serve(state: ApiState) -> u16 {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move { axum::serve(listener, router(state)).await });
    port
}

#[tokio::test]
async fn the_event_stream_tells_an_episode_started_then_the_status() {
    use futures_util::StreamExt;
    use tokio_tungstenite::tungstenite::Message;

    let folder = scratch("events");
    let _capture = FakeCapture::start(&folder.join("capture.sock"), recording);
    let port = serve(state(&folder)).await;
    let (mut socket, _) =
        tokio_tungstenite::connect_async(format!("ws://127.0.0.1:{port}/api/events"))
            .await
            .unwrap();
    let mut next = async || {
        let Some(Ok(Message::Text(text))) = socket.next().await else {
            panic!("no event");
        };
        serde_json::from_str::<Value>(&text).unwrap()
    };
    assert_eq!(
        next().await,
        json!({"event": "episode_started", "episode": "20260310T120000.000Z-1a2b3c4d"})
    );
    let status = next().await;
    assert_eq!(status["event"], "status");
    assert_eq!(status["status"]["capture"]["streams"][0]["written"], 13_990);
}

/// A capture process recording nothing, as its status says.
fn not_recording(request: &Request) -> Response {
    match request {
        Request::Status => Response::Status(Status::default()),
        other => idle(other),
    }
}

#[tokio::test]
async fn an_episode_capture_announces_is_told_though_polling_never_saw_it() {
    use futures_util::StreamExt;
    use kapto_capture::control::Announcement;
    use tokio_tungstenite::tungstenite::Message;

    let folder = scratch("events-announced");
    let _capture = FakeCapture::start(&folder.join("capture.sock"), not_recording);
    let state = state(&folder);
    let announcements = state.announcements.clone();
    let port = serve(state).await;
    let (mut socket, _) =
        tokio_tungstenite::connect_async(format!("ws://127.0.0.1:{port}/api/events"))
            .await
            .unwrap();
    let mut next = async || {
        let Some(Ok(Message::Text(text))) = socket.next().await else {
            panic!("no event");
        };
        serde_json::from_str::<Value>(&text).unwrap()
    };
    // Listening once the first status has come.
    assert_eq!(next().await["event"], "status");
    let episode = "20260310T120000.000Z-1a2b3c4d";
    announcements.announce(Announcement::EpisodeStarted {
        episode: episode.into(),
    });
    announcements.announce(Announcement::EpisodeEnded {
        episode: episode.into(),
        finalised: true,
    });
    assert_eq!(
        next().await,
        json!({"event": "episode_started", "episode": episode})
    );
    assert_eq!(
        next().await,
        json!({"event": "episode_ended", "episode": episode, "finalised": true})
    );
}

#[tokio::test]
async fn another_site_s_page_may_not_open_the_event_stream() {
    use tokio_tungstenite::tungstenite::Error;
    use tokio_tungstenite::tungstenite::client::IntoClientRequest;

    let folder = scratch("events-origin");
    let port = serve(state(&folder)).await;
    let mut request = format!("ws://127.0.0.1:{port}/api/events")
        .into_client_request()
        .unwrap();
    request
        .headers_mut()
        .insert(header::ORIGIN, "http://attacker.example".parse().unwrap());
    let Err(Error::Http(response)) = tokio_tungstenite::connect_async(request).await else {
        panic!("the connection was accepted");
    };
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    let body: Value = serde_json::from_slice(response.body().as_deref().unwrap()).unwrap();
    assert_eq!(body["code"], "api.origin_not_allowed");
}

/// Asks for episode `id`'s replay through the router.
async fn ask_replay(state: ApiState, id: &str) -> (StatusCode, Value) {
    let uri = format!("/api/episodes/{id}/replay");
    call(state, "POST", &uri, Some(("application/json", "{}"))).await
}

/// Where episode `id`'s replay is, through the router.
async fn replay_status(state: ApiState, id: &str) -> Value {
    let (status, body) = call(state, "GET", &format!("/api/episodes/{id}/replay"), None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    body
}

/// Does the replay job the API queued, as the worker would: makes its view
/// in the cache and reports it done with `warnings`. Jobs queued before it,
/// such as reading the episode's events, are done first.
fn made(state: &ApiState, id: &str, warnings: Vec<JobFailure>) -> std::path::PathBuf {
    let job = loop {
        let job = state.store.claim().unwrap().unwrap();
        if job.kind == REPLAY {
            break job;
        }
        state.store.finish(job.id, None, Vec::new()).unwrap();
    };
    assert_eq!((job.kind.as_str(), job.episode.as_str()), (REPLAY, id));
    view(state, id, VIEW_VERSION);
    state.store.finish(job.id, None, warnings).unwrap();
    view_file(&state.data_dir, id).unwrap()
}

/// Writes the start of episode `id`'s replay view, of version `version`.
fn view(state: &ApiState, id: &str, version: u16) {
    let mut start = VIEW_MAGIC.to_vec();
    start.extend(version.to_le_bytes());
    let path = view_file(&state.data_dir, id).unwrap();
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, start).unwrap();
}

#[tokio::test]
async fn a_replay_whose_view_is_of_another_version_is_made_again_when_asked_for() {
    let folder = scratch("replay-other-version");
    let id = "20260310T120000.000Z-00000001";
    finalised_episode(&folder.join("data"), id, 0);
    let api = state(&folder);
    ask_replay(api.clone(), id).await;
    made(&api, id, Vec::new());
    assert_eq!(replay_status(api.clone(), id).await["state"], "ready");

    view(&api, id, VIEW_VERSION + 1);
    assert_eq!(replay_status(api.clone(), id).await["state"], "not_made");
    let (status, body) = ask_replay(api.clone(), id).await;
    assert_eq!(
        (status, &body["state"]),
        (StatusCode::ACCEPTED, &json!("queued"))
    );
}

#[tokio::test]
async fn a_replay_asked_for_twice_is_made_once_saying_what_it_lacks() {
    let folder = scratch("replay-asked");
    let id = "20260310T120000.000Z-00000001";
    finalised_episode(&folder.join("data"), id, 0);
    let api = state(&folder);
    assert_eq!(replay_status(api.clone(), id).await["state"], "not_made");

    let (status, body) = ask_replay(api.clone(), id).await;
    assert_eq!(
        (status, &body["state"]),
        (StatusCode::ACCEPTED, &json!("queued"))
    );
    let (status, _) = ask_replay(api.clone(), id).await;
    assert_eq!(status, StatusCode::ACCEPTED);
    // Queued once.
    let replays = api.store.reader().unwrap().jobs(10).unwrap();
    assert_eq!(replays.iter().filter(|job| job.kind == REPLAY).count(), 1);

    let without_robot = JobFailure::from(&KaptoError::new(ErrorCode::ViewerReplayWithoutRobot));
    made(&api, id, vec![without_robot]);
    let ready = replay_status(api.clone(), id).await;
    assert_eq!(ready["state"], "ready");
    // The view's size: its magic and version.
    assert_eq!(ready["bytes"], 6);
    assert_eq!(ready["lacks"][0]["code"], "viewer.replay_without_robot");
    assert_eq!(
        ready["lacks"][0]["remedy"],
        ErrorCode::ViewerReplayWithoutRobot.remedy()
    );
    // Made, asking again queues nothing.
    let (status, body) = ask_replay(api.clone(), id).await;
    assert_eq!((status, &body["state"]), (StatusCode::OK, &json!("ready")));
    assert_eq!(api.store.claim().unwrap(), None);
}

#[tokio::test]
async fn a_replay_that_failed_or_left_the_cache_is_made_again_when_asked_for() {
    let folder = scratch("replay-again");
    let id = "20260310T120000.000Z-00000001";
    finalised_episode(&folder.join("data"), id, 0);
    let api = state(&folder);
    ask_replay(api.clone(), id).await;
    let path = made(&api, id, Vec::new());
    // Removed from the cache: not made, until asked for again.
    std::fs::remove_file(&path).unwrap();
    assert_eq!(replay_status(api.clone(), id).await["state"], "not_made");
    let (status, body) = ask_replay(api.clone(), id).await;
    assert_eq!(
        (status, &body["state"]),
        (StatusCode::ACCEPTED, &json!("queued"))
    );

    // Failing, it says why; asked for again, it is tried again.
    let job = api.store.claim().unwrap().unwrap();
    assert_eq!(replay_status(api.clone(), id).await["state"], "making");
    let low = KaptoError::new(ErrorCode::ViewerReplayDiskLow).detail("9 MiB free");
    api.store
        .finish(job.id, Some(JobFailure::from(&low)), Vec::new())
        .unwrap();
    let failed = replay_status(api.clone(), id).await;
    assert_eq!(failed["state"], "failed");
    assert_eq!(failed["failure"]["code"], "viewer.replay_disk_low");
    assert_eq!(failed["failure"]["message"], low.message());
    assert_eq!(
        failed["failure"]["remedy"],
        ErrorCode::ViewerReplayDiskLow.remedy()
    );
    let (_, body) = ask_replay(api.clone(), id).await;
    assert_eq!(
        (&body["state"], &body["failure"]),
        (&json!("queued"), &Value::Null)
    );
}

#[tokio::test]
async fn a_replay_not_made_or_of_an_episode_that_isnt_there_is_refused_with_its_cause() {
    let folder = scratch("replay-refused");
    let id = "20260310T120000.000Z-00000001";
    finalised_episode(&folder.join("data"), id, 0);
    let (status, body) = call(
        state(&folder),
        "GET",
        &format!("/api/episodes/{id}/replay/opening"),
        None,
    )
    .await;
    assert_eq!(
        (status, &body["code"]),
        (StatusCode::NOT_FOUND, &json!("viewer.replay_not_ready"))
    );
    assert_eq!(body["remedy"], ErrorCode::ViewerReplayNotReady.remedy());
    let other = "20260310T120000.000Z-0000000f";
    for uri in [
        format!("/api/episodes/{other}/replay"),
        format!("/api/episodes/{other}/replay/opening"),
    ] {
        let (status, body) = call(state(&folder), "GET", &uri, None).await;
        assert_eq!(
            (status, &body["code"]),
            (StatusCode::NOT_FOUND, &json!("api.episode_not_found")),
            "{uri}"
        );
    }
    let (status, body) = ask_replay(state(&folder), other).await;
    assert_eq!(
        (status, &body["code"]),
        (StatusCode::NOT_FOUND, &json!("api.episode_not_found"))
    );
    let (status, body) = ask_replay(state(&folder), "..%2Fkapto.db").await;
    assert_eq!(
        (status, &body["code"]),
        (StatusCode::BAD_REQUEST, &json!("api.invalid_request"))
    );
}

/// A mesh file of one triangle.
const TRIANGLE_STL: &str = "solid t\nfacet normal 0 0 1\n outer loop\n  vertex 0 0 0\n  vertex 1 0 0\n  vertex 0 1 0\n endloop\nendfacet\nendsolid t\n";

/// A robot in `folder`: a red box on its base, a mesh on its arm, the same
/// mesh, a sphere and a mesh file that isn't there on its hand.
fn cell(folder: &Path) -> kapto_robot::RobotModel {
    std::fs::create_dir_all(folder).unwrap();
    std::fs::write(folder.join("part.stl"), TRIANGLE_STL).unwrap();
    let urdf = folder.join("cell.urdf");
    std::fs::write(
        &urdf,
        r#"<robot name="cell">
  <link name="base"><visual><geometry><box size="0.1 0.2 0.3"/></geometry>
    <material name="red"><color rgba="1 0 0 1"/></material></visual></link>
  <link name="arm"><visual><origin xyz="0 0 0.5"/>
    <geometry><mesh filename="part.stl" scale="0.001 0.001 0.001"/></geometry></visual></link>
  <link name="hand">
    <visual><geometry><mesh filename="part.stl"/></geometry></visual>
    <visual><geometry><sphere radius="0.05"/></geometry></visual>
    <visual><geometry><mesh filename="missing.stl"/></geometry></visual>
  </link>
  <joint name="lift" type="fixed"><parent link="base"/><child link="arm"/></joint>
  <joint name="turn" type="revolute"><parent link="arm"/><child link="hand"/>
    <axis xyz="0 0 1"/><limit lower="-1" upper="1" effort="1" velocity="1"/></joint>
</robot>"#,
    )
    .unwrap();
    kapto_robot::load_urdf(&urdf, &[]).unwrap()
}

/// Sends a GET through the router and returns the status, content type and
/// body as it came.
async fn get_bytes(state: ApiState, uri: &str) -> (StatusCode, String, Vec<u8>) {
    let response = router(state)
        .oneshot(HttpRequest::get(uri).body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let kind = response
        .headers()
        .get(header::CONTENT_TYPE)
        .map(|value| value.to_str().unwrap().to_owned())
        .unwrap_or_default();
    let bytes = axum::body::to_bytes(response.into_body(), 1 << 20)
        .await
        .unwrap();
    (status, kind, bytes.to_vec())
}

#[tokio::test]
async fn the_robot_s_scene_lists_each_link_in_order_with_its_shapes_and_each_mesh_file_once() {
    let folder = scratch("scene");
    let state = ApiState {
        scene: Arc::new(Scene::of(&cell(&folder.join("robot")))),
        ..state(&folder)
    };
    let (status, scene) = call(state, "GET", "/api/robot/scene", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        (&scene["name"], &scene["root"]),
        (&json!("cell"), &json!("base"))
    );
    let links: Vec<&str> = scene["links"]
        .as_array()
        .unwrap()
        .iter()
        .map(|link| link["name"].as_str().unwrap())
        .collect();
    assert_eq!(links, ["base", "arm", "hand"]);
    assert_eq!(
        scene["links"][0]["visuals"][0],
        json!({
            "origin": {"translation": [0.0, 0.0, 0.0], "rotation": [0.0, 0.0, 0.0, 1.0]},
            "color": [1.0, 0.0, 0.0, 1.0],
            "shape": {"kind": "box", "size": [0.1, 0.2, 0.3]}
        })
    );
    let arm = &scene["links"][1]["visuals"][0];
    assert_eq!(arm["origin"]["translation"], json!([0.0, 0.0, 0.5]));
    assert_eq!(
        arm["shape"],
        json!({"kind": "mesh", "mesh": 0, "scale": [0.001, 0.001, 0.001]})
    );
    // The same file is the same mesh; the missing one is left out.
    let hand: Vec<&Value> = scene["links"][2]["visuals"]
        .as_array()
        .unwrap()
        .iter()
        .map(|visual| &visual["shape"])
        .collect();
    assert_eq!(
        hand,
        [
            &json!({"kind": "mesh", "mesh": 0, "scale": [1.0, 1.0, 1.0]}),
            &json!({"kind": "sphere", "radius": 0.05})
        ]
    );
    assert_eq!(scene["meshes"], 1);
    // How far it reaches: around the base's box at least.
    let center: Vec<f64> = scene["reach"]["center"]
        .as_array()
        .unwrap()
        .iter()
        .map(|value| value.as_f64().unwrap())
        .collect();
    let radius = scene["reach"]["radius"].as_f64().unwrap();
    for corner in [[0.05, 0.1, 0.15], [-0.05, -0.1, -0.15]] {
        let away: f64 = corner
            .iter()
            .zip(&center)
            .map(|(a, b)| (a - b).powi(2))
            .sum::<f64>()
            .sqrt();
        assert!(away <= radius, "{corner:?} outside {center:?} {radius}");
    }
}

#[tokio::test]
async fn a_mesh_is_sent_as_the_triangles_the_mesh_loader_reads() {
    let folder = scratch("mesh");
    let state = ApiState {
        scene: Arc::new(Scene::of(&cell(&folder.join("robot")))),
        ..state(&folder)
    };
    let (status, kind, bytes) = get_bytes(state, "/api/robot/meshes/0").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(kind, "application/octet-stream");
    let read = kapto_robot::load_mesh(&folder.join("robot/part.stl")).unwrap();
    let word = |at: usize| u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap());
    assert_eq!(&bytes[..4], MESH_MAGIC);
    assert_eq!(word(8) as usize, read.meshes.len());
    let part = &read.meshes[0];
    assert_eq!(
        (word(12) as usize, word(16) as usize),
        (part.positions.len(), part.triangles.len())
    );
    let positions: Vec<f32> = bytes[40..40 + part.positions.len() * 12]
        .as_chunks::<4>()
        .0
        .iter()
        .map(|value| f32::from_le_bytes(*value))
        .collect();
    assert_eq!(positions, part.positions.concat());
    let triangles = &bytes[bytes.len() - part.triangles.len() * 12..];
    let indices: Vec<u32> = triangles
        .as_chunks::<4>()
        .0
        .iter()
        .map(|value| u32::from_le_bytes(*value))
        .collect();
    assert_eq!(indices, part.triangles.concat());
}

#[tokio::test]
async fn a_mesh_the_scene_lacks_or_can_t_read_is_refused_with_its_cause() {
    let folder = scratch("mesh-refused");
    let scene = Arc::new(Scene::of(&cell(&folder.join("robot"))));
    let state = || ApiState {
        scene: Arc::clone(&scene),
        ..state(&folder)
    };
    for (uri, expected, code) in [
        (
            "/api/robot/meshes/1",
            StatusCode::NOT_FOUND,
            "viewer.mesh_not_found",
        ),
        (
            "/api/robot/meshes/first",
            StatusCode::BAD_REQUEST,
            "api.invalid_request",
        ),
    ] {
        let (status, body) = call(state(), "GET", uri, None).await;
        assert_eq!(
            (status, body["code"].as_str()),
            (expected, Some(code)),
            "{uri}"
        );
    }
    // Gone since serve loaded the robot.
    std::fs::remove_file(folder.join("robot/part.stl")).unwrap();
    let (status, body) = call(state(), "GET", "/api/robot/meshes/0", None).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["code"], "robot.mesh_unreadable");
}

/// Writes episode `id`'s replay view: a message every millisecond of its
/// one second, from `START`, its value the millisecond.
fn viewed(state: &ApiState, id: &str) {
    viewed_with(state, id, ViewContents::default());
}

/// The same, with the robot and meshes in `contents`.
fn viewed_with(state: &ApiState, id: &str, contents: ViewContents) {
    let path = view_file(&state.data_dir, id).unwrap();
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let mut writer = ViewWriter::create(&path, 16_666_666).unwrap();
    for ms in 0..1000_u32 {
        let time = START + i64::from(ms) * 1_000_000;
        writer.message(0, time, &[f64::from(ms)]).unwrap();
    }
    let value = |name: &str, plotted| OpeningValue {
        name: name.to_owned(),
        plotted,
    };
    let streams = vec![OpeningStream {
        name: "arm_joints".to_owned(),
        values: vec![value("sequence", false), value("joint_positions[0]", true)],
    }];
    writer
        .finish(ViewContents {
            links: vec!["base_link".to_owned(), "link_1".to_owned()],
            streams,
            ..contents
        })
        .unwrap();
}

/// When the episodes [`finalised_episode`] makes at second 0 begin.
const START: i64 = 1_772_700_400_000_000_000;

/// A stretch of episode `id`'s replay through the router: its status and
/// body.
async fn window(state: ApiState, id: &str, query: &str) -> (StatusCode, Vec<u8>) {
    let request = HttpRequest::get(format!("/api/episodes/{id}/replay/window?{query}"))
        .body(Body::empty())
        .unwrap();
    let response = router(state).oneshot(request).await.unwrap();
    let status = response.status();
    let body = axum::body::to_bytes(response.into_body(), 1 << 20)
        .await
        .unwrap();
    (status, body.to_vec())
}

fn code(body: &[u8]) -> String {
    serde_json::from_slice::<Value>(body).unwrap()["code"]
        .as_str()
        .unwrap()
        .to_owned()
}

#[tokio::test]
async fn a_stretch_of_a_replay_is_its_view_merged_into_the_points_asked_for() {
    let folder = scratch("replay-window");
    let id = "20260310T120000.000Z-00000001";
    finalised_episode(&folder.join("data"), id, 0);
    let api = state(&folder);
    viewed(&api, id);
    let (status, body) = window(
        api,
        id,
        &format!("from={START}&to={}&points=10", START + 999_000_000),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(&body[..4], b"KVHI");
    // The span is the episode's, whatever was asked for.
    let at = |offset: usize| i64::from_le_bytes(body[offset..offset + 8].try_into().unwrap());
    assert_eq!((at(12), at(20)), (START, START + 999_000_000));
    // No poses, one stream in ten parts.
    let count = |offset: usize| u32::from_le_bytes(body[offset..offset + 4].try_into().unwrap());
    assert_eq!((count(28), count(32), count(36)), (0, 0, 1));
    assert_eq!(count(44), 1, "one value");
    assert_eq!(count(48), 10, "ten parts");
}

#[tokio::test]
async fn a_stretch_of_a_replay_not_made_unreadable_or_outside_its_episode_is_refused() {
    let folder = scratch("replay-window-refused");
    let id = "20260310T120000.000Z-00000001";
    finalised_episode(&folder.join("data"), id, 0);
    let api = state(&folder);
    let whole = format!("from={START}&to={}&points=10", START + 999_000_000);
    let (status, body) = window(api.clone(), id, &whole).await;
    assert_eq!(
        (status, code(&body)),
        (StatusCode::NOT_FOUND, "viewer.replay_not_ready".to_owned())
    );

    // Of this version, but cut off after its start.
    view(&api, id, VIEW_VERSION);
    let (status, body) = window(api.clone(), id, &whole).await;
    assert_eq!(
        (status, code(&body)),
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            "viewer.replay_unreadable".to_owned()
        )
    );

    viewed(&api, id);
    for query in [
        format!("from={START}&to={}&points=10", START - 1),
        format!(
            "from={}&to={}&points=10",
            START + 2_000_000_000,
            START + 3_000_000_000
        ),
        format!("from={START}&to={START}&points=0"),
        format!("from={START}&to={START}&points=10001"),
        format!("from={START}&points=10"),
        "from=soon&to=later&points=10".to_owned(),
    ] {
        let (status, body) = window(api.clone(), id, &query).await;
        assert_eq!(
            (status, code(&body)),
            (StatusCode::BAD_REQUEST, "api.invalid_request".to_owned()),
            "{query}"
        );
    }
    let (status, body) = window(api, "20260310T120000.000Z-0000000f", &whole).await;
    assert_eq!(
        (status, code(&body)),
        (StatusCode::NOT_FOUND, "api.episode_not_found".to_owned())
    );
}

#[tokio::test]
async fn a_replays_robot_and_meshes_are_the_ones_its_view_carries() {
    let folder = scratch("replay-scene");
    let id = "20260310T120000.000Z-00000001";
    finalised_episode(&folder.join("data"), id, 0);
    let api = state(&folder);
    let scene_uri = format!("/api/episodes/{id}/replay/scene");
    let mesh_uri = |n: &str| format!("/api/episodes/{id}/replay/meshes/{n}");
    let (status, body) = call(api.clone(), "GET", &scene_uri, None).await;
    assert_eq!(
        (status, &body["code"]),
        (StatusCode::NOT_FOUND, &json!("viewer.replay_not_ready"))
    );

    // Recorded without a robot.
    viewed(&api, id);
    let (status, body) = call(api.clone(), "GET", &scene_uri, None).await;
    assert_eq!(
        (status, &body["code"]),
        (StatusCode::NOT_FOUND, &json!("viewer.replay_without_robot"))
    );

    // Recorded with a robot whose second mesh couldn't be read.
    let scene = RobotScene {
        name: "arm7".to_owned(),
        root: "base_link".to_owned(),
        meshes: 2,
        ..RobotScene::default()
    };
    viewed_with(
        &api,
        id,
        ViewContents {
            scene: Some(scene),
            meshes: vec![b"KMSH a mesh".to_vec(), Vec::new()],
            ..ViewContents::default()
        },
    );
    let (status, body) = call(api.clone(), "GET", &scene_uri, None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        body,
        json!({"name": "arm7", "root": "base_link", "links": [], "meshes": 2})
    );
    let (status, kind, bytes) = get_bytes(api.clone(), &mesh_uri("0")).await;
    assert_eq!(
        (status, kind.as_str(), bytes.as_slice()),
        (
            StatusCode::OK,
            "application/octet-stream",
            &b"KMSH a mesh"[..]
        )
    );
    for (n, expected, cause) in [
        ("1", StatusCode::CONFLICT, "robot.mesh_unreadable"),
        ("2", StatusCode::NOT_FOUND, "viewer.mesh_not_found"),
        ("first", StatusCode::BAD_REQUEST, "api.invalid_request"),
    ] {
        let (status, body) = call(api.clone(), "GET", &mesh_uri(n), None).await;
        assert_eq!((status, &body["code"]), (expected, &json!(cause)), "{n}");
    }
}

#[tokio::test]
async fn a_replays_opening_names_its_links_and_values_and_gives_its_span_once_made() {
    let folder = scratch("replay-opening");
    let id = "20260310T120000.000Z-00000001";
    finalised_episode(&folder.join("data"), id, 0);
    let api = state(&folder);
    let uri = format!("/api/episodes/{id}/replay/opening");
    let (status, body) = call(api.clone(), "GET", &uri, None).await;
    assert_eq!(
        (status, &body["code"]),
        (StatusCode::NOT_FOUND, &json!("viewer.replay_not_ready"))
    );

    viewed(&api, id);
    let (status, body) = call(api.clone(), "GET", &uri, None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        body,
        json!({
            "start_utc_ns": START,
            "end_utc_ns": START + 999_000_000,
            "links": ["base_link", "link_1"],
            "streams": [{
                "name": "arm_joints",
                "values": [
                    {"name": "sequence", "plotted": false},
                    {"name": "joint_positions[0]", "plotted": true}
                ]
            }]
        })
    );

    view(&api, id, VIEW_VERSION);
    let (status, body) = call(api, "GET", &uri, None).await;
    assert_eq!(
        (status, &body["code"]),
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            &json!("viewer.replay_unreadable")
        )
    );
}

/// The API's state with destinations `nas`, a folder in `folder`, and
/// `cloud`, whose credentials are named but not given.
fn uploading_state(folder: &Path) -> ApiState {
    let file = kapto_config::ConfigFile {
        path: folder.join("kapto.toml"),
        contents: format!(
            "[[upload.destination]]\nname = \"nas\"\nurl = \"file://{}\"\nprefix = \"fleet-a\"\n\
             [[upload.destination]]\nname = \"cloud\"\nurl = \"s3://cloud-bucket\"\n\
             credentials = {{ env = \"KAPTO_TEST_NEVER_SET\" }}\n",
            folder.join("nas").display()
        ),
    };
    let inputs = kapto_config::Inputs {
        file: Some(&file),
        env: &|_| None,
        cli: &[],
    };
    ApiState {
        destinations: kapto_config::DeploymentConfig::resolve(&inputs)
            .unwrap()
            .upload_destinations,
        ..state(folder)
    }
}

#[tokio::test]
async fn uploads_are_shown_by_destination_and_by_episode_with_where_each_object_went() {
    let folder = scratch("uploads");
    let data = folder.join("data");
    let (going, failed) = (
        "20260310T120000.000Z-00000001",
        "20260310T120100.000Z-00000002",
    );
    finalised_episode(&data, going, 0);
    finalised_episode(&data, failed, 60);
    let state = uploading_state(&folder);
    let store = state.store.clone();
    // `going` is up at `nas` and halfway at `cloud`; `failed` failed at `nas`.
    store.start_upload(going, "nas", "fleet-a/one").unwrap();
    let object = UploadedObject {
        path: "recording.mcap".into(),
        key: "fleet-a/one/recording.mcap".into(),
        bytes: 15,
        sha256: "a".repeat(64),
        e_tag: None,
    };
    store.uploaded(going, "nas", vec![object]).unwrap();
    store.start_upload(going, "cloud", "fleet-b/one").unwrap();
    let counts = UploadCounts {
        bytes_sent: 8,
        bytes_total: 16,
        throttled: 3,
    };
    store.upload_progress(going, "cloud", "{}", counts).unwrap();
    store.start_upload(failed, "nas", "fleet-a/two").unwrap();
    let failure = JobFailure {
        code: "upload.unreachable".into(),
        message: "connection reset".into(),
    };
    store.upload_failed(failed, "nas", failure).unwrap();

    let (status, overview) = call(state, "GET", "/api/uploads", None).await;
    assert_eq!(status, StatusCode::OK, "{overview}");
    let nas = &overview["destinations"][0];
    assert_eq!(
        (
            nas["name"].clone(),
            nas["url"].clone(),
            nas["uploaded"].clone(),
            nas["failed"].clone()
        ),
        (
            json!("nas"),
            json!(format!("file://{}", folder.join("nas").display())),
            json!(1),
            json!(1)
        )
    );
    let cloud = &overview["destinations"][1];
    assert_eq!(
        (
            cloud["uploading"].clone(),
            cloud["bytes_sent"].clone(),
            cloud["bytes_total"].clone()
        ),
        (json!(1), json!(8), json!(16))
    );
    assert_eq!(cloud["throttled"], 3);
    // Only those not done, each with why it failed if it did.
    let listed = overview["uploads"].as_array().unwrap();
    assert_eq!(listed.len(), 2, "{overview}");
    let failed_one = listed
        .iter()
        .find(|upload| upload["state"] == "failed")
        .unwrap();
    assert_eq!(failed_one["failure_code"], "upload.unreachable");

    let (status, uploads) = call(
        uploading_state(&folder),
        "GET",
        &format!("/api/episodes/{going}/uploads"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{uploads}");
    assert_eq!(uploads["stored_here"], true);
    assert_eq!(uploads["deletion"], Value::Null);
    let at_nas = uploads["uploads"]
        .as_array()
        .unwrap()
        .iter()
        .find(|upload| upload["destination"] == "nas")
        .unwrap();
    assert_eq!(at_nas["objects"][0]["key"], "fleet-a/one/recording.mcap");
}

#[tokio::test]
async fn an_episode_deleted_here_is_still_listed_and_shown_with_where_its_copies_and_its_results_are()
 {
    let folder = scratch("uploads-deleted");
    let data = folder.join("data");
    let id = "20260310T120000.000Z-00000001";
    finalised_episode(&data, id, 0);
    let state = uploading_state(&folder);
    let store = state.store.clone();
    store.start_upload(id, "nas", "fleet-a/one").unwrap();
    let object = UploadedObject {
        path: "recording.mcap".into(),
        key: "fleet-a/one/recording.mcap".into(),
        bytes: 15,
        sha256: "a".repeat(64),
        e_tag: None,
    };
    store.uploaded(id, "nas", vec![object]).unwrap();
    let summary = std::fs::read_to_string(data.join(id).join("episode.json")).unwrap();
    store
        .record_deletion(Deletion {
            episode: id.to_owned(),
            deleted_utc_ns: 0,
            reason: DeletionReason::AfterUpload,
            deleted_by: "worker".into(),
            copies: vec![RemoteCopy {
                destination: "nas".into(),
                key: "fleet-a/one".into(),
            }],
            summary,
        })
        .unwrap();
    std::fs::remove_dir_all(data.join(id)).unwrap();
    drop(state);

    // Listed after the index is rebuilt, as stored only remotely.
    let (_, list) = call(uploading_state(&folder), "GET", "/api/episodes", None).await;
    assert_eq!(
        (list[0]["id"].clone(), list[0]["stored_here"].clone()),
        (json!(id), json!(false))
    );
    // Shown from the summary kept, its files those uploaded.
    let (status, detail) = call(
        uploading_state(&folder),
        "GET",
        &format!("/api/episodes/{id}"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{detail}");
    assert_eq!(detail["stored_here"], false);
    assert_eq!(
        detail["files"],
        json!([{"path": "recording.mcap", "bytes": 15}])
    );
    let (_, uploads) = call(
        uploading_state(&folder),
        "GET",
        &format!("/api/episodes/{id}/uploads"),
        None,
    )
    .await;
    assert_eq!(uploads["stored_here"], false);
    assert_eq!(
        (
            uploads["deletion"]["reason"].clone(),
            uploads["deletion"]["destinations"].clone()
        ),
        (json!("after_upload"), json!(["nas"]))
    );
    // What was worked out from it is still shown, kept in kapto.db; its
    // replay, made from its recording, can't be.
    for path in ["qa", "statistics", "events"] {
        let (status, answer) = call(
            uploading_state(&folder),
            "GET",
            &format!("/api/episodes/{id}/{path}"),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{path}: {answer}");
    }
    let (status, _) = call(
        uploading_state(&folder),
        "GET",
        &format!("/api/episodes/{id}/replay"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    // An episode neither here nor deleted here isn't found.
    let unknown = "20260310T130000.000Z-00000009";
    let (status, error) = call(
        uploading_state(&folder),
        "GET",
        &format!("/api/episodes/{unknown}/uploads"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(error["code"], "api.episode_not_found");
}

#[tokio::test]
async fn failed_uploads_are_tried_again_when_asked_and_asking_with_none_failed_is_refused() {
    let folder = scratch("uploads-retry");
    let data = folder.join("data");
    let id = "20260310T120000.000Z-00000001";
    finalised_episode(&data, id, 0);
    let state = uploading_state(&folder);
    let store = state.store.clone();
    store.start_upload(id, "nas", "fleet-a/one").unwrap();
    let uri = format!("/api/episodes/{id}/uploads/retry");
    let (status, error) = call(state, "POST", &uri, None).await;
    assert_eq!(
        (status, error["code"].clone()),
        (StatusCode::CONFLICT, json!("upload.nothing_to_retry"))
    );

    let failure = JobFailure {
        code: "upload.unreachable".into(),
        message: "connection reset".into(),
    };
    store.upload_failed(id, "nas", failure).unwrap();
    let (status, retried) = call(uploading_state(&folder), "POST", &uri, None).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{retried}");
    assert_eq!(retried, json!({"destinations": ["nas"]}));
    let job = store.reader().unwrap().job("upload", id).unwrap().unwrap();
    assert_eq!(job.state, JobState::Queued);

    let (status, _) = call(
        uploading_state(&folder),
        "POST",
        "/api/episodes/..%2Fx/uploads/retry",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn a_destination_is_tested_as_an_upload_uses_it_and_one_that_cant_be_says_why() {
    let folder = scratch("uploads-test");
    let test = |name: &str| format!("/api/uploads/destinations/{name}/test");
    let (status, tested) = call(uploading_state(&folder), "POST", &test("nas"), None).await;
    assert_eq!(status, StatusCode::OK, "{tested}");
    assert_eq!(tested["removed"], true);
    assert!(
        tested["key"]
            .as_str()
            .unwrap()
            .starts_with("fleet-a/.kapto-probe-"),
        "{tested}"
    );
    // Nothing left behind.
    assert_eq!(
        std::fs::read_dir(folder.join("nas").join("fleet-a"))
            .unwrap()
            .count(),
        0
    );

    let (status, error) = call(uploading_state(&folder), "POST", &test("cloud"), None).await;
    assert_eq!(
        (status, error["code"].clone()),
        (StatusCode::CONFLICT, json!("upload.credentials_missing")),
        "{error}"
    );
    let (status, error) = call(uploading_state(&folder), "POST", &test("elsewhere"), None).await;
    assert_eq!(
        (status, error["code"].clone()),
        (StatusCode::NOT_FOUND, json!("upload.no_destination"))
    );
}
