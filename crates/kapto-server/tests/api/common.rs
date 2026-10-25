// SPDX-License-Identifier: Apache-2.0

//! What the API's tests share: the state, a stand-in for the capture
//! process, calling the router, and finalised episodes to list.

use std::net::IpAddr;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::thread::JoinHandle;
use std::time::Duration;

use axum::body::Body;
use axum::http::{Request as HttpRequest, StatusCode, header};
use kapto_capture::control::{
    ControlServer, LiveViewCounts, Request, Response, Status, StreamStatus,
};
use kapto_capture::disk::DiskLimits;
use kapto_capture::finalise::Staged;
use kapto_core::{EpisodeId, ErrorCode, KaptoError};
use kapto_server::api::{ApiState, CaptureAnnouncements, router};
use kapto_server::layouts::Layouts;
use kapto_server::robot::RobotOverview;
use kapto_server::scene::Scene;
use kapto_server::settings::SettingsOverview;
use kapto_server::{Processes, index};
use kapto_store::{PackageIdentity, Preflight, PreflightCheck};
use serde_json::Value;
use tower::ServiceExt;

/// The API's state for a data directory `folder/data`, its episode index
/// rebuilt from what the folder holds now.
pub(crate) fn state(folder: &Path) -> ApiState {
    ApiState {
        processes: Arc::new(Processes::new("serve", &["capture", "viz-bridge"])),
        robot: Arc::new(RobotOverview::default()),
        scene: Arc::new(Scene::default()),
        layouts: Layouts::default(),
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
pub(crate) struct FakeCapture {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl FakeCapture {
    pub(crate) fn start(socket: &Path, answer: fn(&Request) -> Response) -> Self {
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
pub(crate) async fn call(
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
pub(crate) fn silent_arm(waived: bool) -> Preflight {
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

pub(crate) fn recording(request: &Request) -> Response {
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
            // The episode before, being closed while this one records.
            closing: Some("20260310T115900.000Z-0a0b0c0d".into()),
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

/// Writes `contents` to `path` inside the test's configuration folder.
pub(crate) fn saved(folder: &Path, path: &str, contents: &str) {
    let path = folder.join("config").join(path);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, contents).unwrap();
}

pub(crate) fn idle(request: &Request) -> Response {
    match request {
        Request::Stop | Request::Abort => {
            Response::error(&KaptoError::new(ErrorCode::CaptureNotRecording))
        }
        other => recording(other),
    }
}

/// A finalised episode in `data` that started at `second` past a fixed time.
pub(crate) fn finalised_episode(data: &Path, id: &str, second: i64) {
    finalised_episode_with(data, id, second, None);
}

/// The same, recorded with `package`, or `unchecked` without.
pub(crate) fn finalised_episode_with(
    data: &Path,
    id: &str,
    second: i64,
    package: Option<PackageIdentity>,
) {
    let id: EpisodeId = id.parse().unwrap();
    let staged = Staged::create(data, &id).unwrap();
    std::fs::write(staged.recording_path(), b"not really mcap").unwrap();
    let start = 1_772_700_400_000_000_000 + second * 1_000_000_000;
    kapto_testkit::episode(
        &id.to_string(),
        (start, start + 1_000_000_000),
        vec![kapto_testkit::stream("arm_joints", 2000)],
    )
    // Started and stopped a fifth of a second inside its span, as a
    // pre-roll and a post-roll leave it.
    .with_commands((start + 200_000_000, Some(start + 800_000_000)))
    .with_package(package)
    .write(&staged.dir())
    .unwrap();
    staged.finalise().unwrap();
}

/// Sends a GET through the router and returns the status, content type and
/// body as it came.
pub(crate) async fn get_bytes(state: ApiState, uri: &str) -> (StatusCode, String, Vec<u8>) {
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
