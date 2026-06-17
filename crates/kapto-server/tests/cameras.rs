// SPDX-License-Identifier: Apache-2.0

//! An episode's cameras through the API: every frame's receive time, and
//! each frame's picture, a compressed one as recorded and a raw one as
//! JPEG, read from a real recording by its indexes; what isn't there or
//! can't be shown refused with its cause.

#![allow(
    clippy::unwrap_used,
    reason = "test helpers fail the test loudly when the setup itself breaks"
)]

use std::net::IpAddr;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::http::{Request as HttpRequest, StatusCode, header};
use kapto_capture::disk::DiskLimits;
use kapto_capture::finalise::Staged;
use kapto_capture::{Batch, ChannelSpec, LossCounters, Recording, StreamIndex, WriterConfig};
use kapto_core::{ClockMap, ClockSample, EpisodeId, ErrorCode, RecvTime};
use kapto_server::Processes;
use kapto_server::api::{ApiState, CaptureAnnouncements, router};
use kapto_server::index;
use kapto_server::layouts::Layouts;
use kapto_server::robot::RobotOverview;
use kapto_server::scene::Scene;
use kapto_server::settings::SettingsOverview;
use kapto_testkit::scratch;
use serde_json::Value;
use tower::ServiceExt;

const ID: &str = "20260310T120000.000Z-00000001";
/// When the recording's monotonic clock reads 0, in UTC nanoseconds.
const START: i64 = 1_772_700_400_000_000_000;

fn state(folder: &Path) -> ApiState {
    ApiState {
        processes: Arc::new(Processes::new("serve", &["capture", "viz-bridge"])),
        robot: Arc::new(RobotOverview::default()),
        scene: Arc::new(Scene::default()),
        layouts: Layouts::default(),
        settings: Arc::new(SettingsOverview::default()),
        config_folder: folder.join("config"),
        package: None,
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

/// The streams the recording carries: an arm, a JPEG camera over TCP whose
/// messages keep their length prefix, and a raw grey one.
const STREAMS: &str = "[streams.arm_joints]\nrate_hz = 30\nudp = { bind = \"127.0.0.1:9000\" }\n\
[streams.wrist_camera]\nrate_hz = 30\nmax_message_bytes = 65536\n\
tcp = { listen = \"127.0.0.1:7010\", framing = { kind = \"length_prefix\", length_bytes = 4 } }\n\
camera = { format = \"jpeg\" }\n\
[streams.depth]\nrate_hz = 30\n\
tcp = { listen = \"127.0.0.1:7011\", framing = { kind = \"fixed\", size = 48 } }\n\
camera = { format = \"raw\", width = 8, height = 6, encoding = \"mono8\" }\n\
[streams.broken]\nrate_hz = 30\n\
tcp = { listen = \"127.0.0.1:7012\", framing = { kind = \"fixed\", size = 8 } }\n\
camera = { format = \"jpeg\" }\n";

/// JPEG `n`, as a camera would send it: its own bytes after the markers.
fn jpeg(n: u8) -> Vec<u8> {
    let mut jpeg = vec![0xff, 0xd8, 0xff, 0xe0];
    jpeg.extend([n; 600]);
    jpeg
}

/// A finalised episode whose recording has ten frames of each camera, a
/// thirtieth of a second apart, beside the arm's messages, in small
/// chunks.
fn recorded(data: &Path) {
    let id: EpisodeId = ID.parse().unwrap();
    let staged = Staged::create(data, &id).unwrap();
    let clock = ClockMap::new(ClockSample {
        mono: RecvTime::from_nanos(0),
        utc_ns: START,
        uncertainty: Duration::ZERO,
    });
    let streams: Vec<ChannelSpec> = ["arm_joints", "wrist_camera", "depth", "broken"]
        .into_iter()
        .zip(0..)
        .map(|(name, index): (&str, StreamIndex)| ChannelSpec::from((index, name.parse().unwrap())))
        .collect();
    let config = WriterConfig {
        chunk_bytes: 2048,
        ..WriterConfig::default()
    };
    let mut recording =
        Recording::create(&staged.recording_path(), &streams, &clock, &config).unwrap();
    recording
        .attach(
            "streams.toml",
            "application/toml",
            START,
            STREAMS.as_bytes(),
        )
        .unwrap();
    let counters = LossCounters::new(4);
    for n in 0..10_u8 {
        let mut batch = Batch::new(4, 1024);
        assert!(batch.push(0, &[n; 8]));
        let mut framed = u32::try_from(jpeg(n).len()).unwrap().to_le_bytes().to_vec();
        framed.extend(jpeg(n));
        assert!(batch.push(1, &framed));
        assert!(batch.push(2, &[n * 20; 48]));
        // Declared JPEG, but not.
        assert!(batch.push(3, b"not jpeg"));
        batch.set_recv_time(RecvTime::from_nanos(u64::from(n) * 33_333_333));
        recording.write(&batch, &counters).unwrap();
    }
    recording.finish(&clock).unwrap();
    kapto_testkit::episode(
        ID,
        (START, START + 300_000_000),
        vec![kapto_testkit::stream("wrist_camera", 30)],
    )
    .write(&staged.dir())
    .unwrap();
    staged.finalise().unwrap();
}

async fn get(state: ApiState, uri: &str) -> (StatusCode, String, Vec<u8>) {
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

async fn json(state: ApiState, uri: &str) -> (StatusCode, Value) {
    let (status, _, bytes) = get(state, uri).await;
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

#[tokio::test]
async fn every_frame_time_of_a_camera_is_listed_in_order() {
    let folder = scratch("camera-frames");
    recorded(&folder.join("data"));
    let (status, body) = json(
        state(&folder),
        &format!("/api/episodes/{ID}/cameras/wrist_camera/frames"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["camera"], "wrist_camera");
    let times: Vec<i64> = body["times_utc_ns"]
        .as_array()
        .unwrap()
        .iter()
        .map(|time| time.as_i64().unwrap())
        .collect();
    let expected: Vec<i64> = (0..10).map(|n| START + n * 33_333_333).collect();
    assert_eq!(times, expected);
}

#[tokio::test]
async fn a_compressed_frame_is_its_jpeg_as_recorded_and_a_raw_one_a_jpeg_of_its_size() {
    let folder = scratch("camera-picture");
    recorded(&folder.join("data"));
    let (status, kind, bytes) = get(
        state(&folder),
        &format!("/api/episodes/{ID}/cameras/wrist_camera/frames/7"),
    )
    .await;
    assert_eq!((status, kind.as_str()), (StatusCode::OK, "image/jpeg"));
    assert_eq!(bytes, jpeg(7), "after the length prefix, byte for byte");

    let (status, kind, bytes) = get(
        state(&folder),
        &format!("/api/episodes/{ID}/cameras/depth/frames/3"),
    )
    .await;
    assert_eq!((status, kind.as_str()), (StatusCode::OK, "image/jpeg"));
    let mut decoder = jpeg_decoder::Decoder::new(bytes.as_slice());
    let pixels = decoder.decode().unwrap();
    let info = decoder.info().unwrap();
    assert_eq!((info.width, info.height), (8, 6));
    assert!(
        pixels.iter().all(|&grey| grey.abs_diff(60) <= 8),
        "{pixels:?}"
    );
}

#[tokio::test]
async fn a_camera_or_frame_that_isnt_there_or_cant_be_shown_is_refused_with_its_cause() {
    let folder = scratch("camera-refused");
    recorded(&folder.join("data"));
    let refused = |uri: String| {
        let state = state(&folder);
        async move { json(state, &uri).await }
    };
    let (status, body) = refused(format!("/api/episodes/{ID}/cameras/arm_joints/frames")).await;
    assert_eq!(
        (status, &body["code"]),
        (
            StatusCode::NOT_FOUND,
            &Value::from("viewer.camera_not_found")
        )
    );
    let (status, body) =
        refused(format!("/api/episodes/{ID}/cameras/wrist_camera/frames/10")).await;
    assert_eq!(
        (status, &body["code"]),
        (
            StatusCode::NOT_FOUND,
            &Value::from("viewer.camera_not_found")
        )
    );
    assert!(
        body["message"].as_str().unwrap().contains("has 10 frames"),
        "{body}"
    );
    let (status, body) = refused(format!(
        "/api/episodes/{ID}/cameras/wrist_camera/frames/first"
    ))
    .await;
    assert_eq!(
        (status, &body["code"]),
        (StatusCode::BAD_REQUEST, &Value::from("api.invalid_request"))
    );
    let other = "20260310T120000.000Z-0000000f";
    let (status, body) =
        refused(format!("/api/episodes/{other}/cameras/wrist_camera/frames")).await;
    assert_eq!(
        (status, &body["code"]),
        (StatusCode::NOT_FOUND, &Value::from("api.episode_not_found"))
    );
    let (status, body) = refused(format!("/api/episodes/{ID}/cameras/broken/frames/0")).await;
    assert_eq!(
        (status, &body["code"]),
        (StatusCode::CONFLICT, &Value::from("viewer.picture_unshown"))
    );
    assert!(
        body["message"]
            .as_str()
            .unwrap()
            .contains("only JPEG and PNG"),
        "{body}"
    );
    assert_eq!(body["remedy"], ErrorCode::ViewerPictureUnshown.remedy());
}
