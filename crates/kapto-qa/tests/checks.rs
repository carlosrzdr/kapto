// SPDX-License-Identifier: Apache-2.0

//! The checks Kapto ships, each on episodes made to pass and to fail it.

#![allow(
    clippy::unwrap_used,
    reason = "test helpers fail the test loudly when the setup itself breaks"
)]

use std::collections::BTreeMap;
use std::path::Path;

use kapto_config::{
    CameraSettings, DurationSettings, FrozenJointsSettings, GripperChatterSettings, IdleSettings,
    JointJumpsSettings, QaSettings, StreamTimingSettings, StreamsCompleteSettings,
    StreamsPresentSettings,
};
use kapto_qa::cameras::CameraFrames;
use kapto_qa::checks::{
    CameraFormat, CameraFrameSizes, EpisodeLong, EpisodeShort, StreamsComplete, StreamsEmpty,
    StreamsGap, StreamsPresent, StreamsRate, StreamsTiming, from_settings,
};
use kapto_qa::conformance::check_meets_the_contract;
use kapto_qa::timing::StreamTiming;
use kapto_qa::{Check, CheckKind, EpisodeView, Verdict};
use kapto_store::StreamSummary;

const ID: &str = "20260305T192345.123Z-1a2b3c4d";

/// A stream that received 2000 messages, of which Kapto dropped `dropped`
/// and 50 more never reached it.
fn stream(id: &str, dropped: u64) -> StreamSummary {
    StreamSummary {
        written: 2000 - dropped,
        dropped_by_us: dropped,
        lost_upstream: 50,
        ..kapto_testkit::stream(id, 2000)
    }
}

fn episode(streams: Vec<StreamSummary>) -> EpisodeView {
    let summary = kapto_testkit::episode(
        ID,
        (1_772_700_400_000_000_000, 1_772_700_460_000_000_000),
        streams,
    );
    EpisodeView::new(Path::new("/data").join(ID).as_path(), summary)
}

#[test]
fn streams_complete_fails_an_episode_from_which_kapto_dropped_messages_naming_the_streams() {
    let dropped = episode(vec![
        stream("arm_joints", 12),
        stream("gripper", 0),
        stream("wrench", 3),
    ]);
    let finding = StreamsComplete::default().run(&dropped).unwrap();
    assert_eq!(StreamsComplete::default().kind(), CheckKind::Gate);
    assert_eq!(finding.verdict, Verdict::Failed);
    assert_eq!(
        finding.message,
        "Kapto dropped 15 messages: arm_joints (12 of 2000 received), wrench (3 of 2000 received)"
    );
    assert_eq!(finding.values["dropped_by_us"], 15.0);
    assert_eq!(finding.values["arm_joints.dropped_by_us"], 12.0);
    assert_eq!(finding.values["gripper.dropped_by_us"], 0.0);
}

#[test]
fn streams_complete_passes_an_episode_with_messages_lost_only_before_they_reached_kapto() {
    let complete = episode(vec![stream("arm_joints", 0), stream("gripper", 0)]);
    let finding = StreamsComplete::default().run(&complete).unwrap();
    assert_eq!(finding.verdict, Verdict::Passed);
    assert_eq!(finding.values["dropped_by_us"], 0.0);

    let empty = StreamsComplete::default()
        .run(&episode(Vec::new()))
        .unwrap();
    assert_eq!(empty.verdict, Verdict::NotApplicable);
}

#[test]
fn streams_complete_meets_the_check_contract() {
    check_meets_the_contract(
        &StreamsComplete::default(),
        &[
            episode(vec![stream("arm_joints", 0)]),
            episode(vec![stream("arm_joints", 7)]),
            episode(Vec::new()),
        ],
    );
}

#[test]
fn the_episode_and_stream_presence_checks_meet_the_check_contract() {
    let episodes = [
        episode(vec![stream("arm_joints", 0)]),
        episode(vec![StreamSummary {
            written: 0,
            ..stream("arm_joints", 0)
        }]),
        episode(Vec::new()),
    ];
    let critical = vec!["arm_joints".to_owned()];
    check_meets_the_contract(&EpisodeShort { min_seconds: 2.0 }, &episodes);
    check_meets_the_contract(&EpisodeLong { max_seconds: 600.0 }, &episodes);
    check_meets_the_contract(&StreamsPresent::default(), &episodes);
    check_meets_the_contract(
        &StreamsPresent {
            critical: critical.clone(),
        },
        &episodes,
    );
    check_meets_the_contract(&StreamsEmpty { critical }, &episodes);
}

#[test]
fn the_stream_timing_checks_meet_the_check_contract() {
    let timed = |messages: &[(i64, &str)]| {
        let mut timing = BTreeMap::new();
        for &(at, name) in messages {
            let stream: &mut StreamTiming = timing.entry(name.to_owned()).or_insert(StreamTiming {
                rate_hz: Some(100),
                messages: 0,
                first_utc_ns: at,
                last_utc_ns: at,
                bins: vec![0; kapto_qa::timing::BINS],
                silences: Vec::new(),
            });
            stream.messages += 1;
            stream.last_utc_ns = at;
            stream.bins[0] += 1;
        }
        episode(vec![stream("arm_joints", 0)]).with_timing(timing)
    };
    let start = 1_772_700_400_000_000_000;
    let episodes = [
        timed(&[(start, "arm_joints"), (start + 10_000_000, "arm_joints")]),
        timed(&[
            (start + 50_000_000_000, "arm_joints"),
            (start + 59_000_000_000, "arm_joints"),
        ]),
        episode(Vec::new()),
    ];
    check_meets_the_contract(
        &StreamsTiming {
            max_start_delay_s: 2.0,
            max_end_gap_s: 5.0,
        },
        &episodes,
    );
    check_meets_the_contract(
        &StreamsRate {
            max_error: 0.15,
            min_end_ratio: 0.5,
        },
        &episodes,
    );
    check_meets_the_contract(&StreamsGap { max_seconds: 2.0 }, &episodes);
}

#[test]
fn the_camera_checks_meet_the_check_contract() {
    let camera = |sizes: &[u64], first_unreadable: Option<&str>| {
        let frames = CameraFrames {
            times_utc_ns: (0..).take(sizes.len()).collect(),
            sizes: sizes.to_vec(),
            first_unreadable: first_unreadable.map(str::to_owned),
            ..CameraFrames::default()
        };
        episode(vec![stream("wrist", 0)])
            .with_cameras([("wrist".to_owned(), frames)].into_iter().collect())
    };
    let episodes = [
        camera(&[1000; 12], None),
        camera(
            &[
                1000, 1000, 1000, 1000, 1000, 1000, 1000, 1000, 1000, 1000, 9,
            ],
            None,
        ),
        camera(&[1000; 3], Some("pictures in `jpeg`, neither JPEG nor PNG")),
        episode(Vec::new()),
    ];
    check_meets_the_contract(&CameraFormat, &episodes);
    check_meets_the_contract(
        &CameraFrameSizes {
            min_size_ratio: 0.3,
        },
        &episodes,
    );
}

#[test]
fn streams_complete_allows_up_to_its_threshold_per_stream() {
    let episode = episode(vec![stream("arm_joints", 12), stream("gripper", 3)]);
    let lenient = StreamsComplete { max_dropped: 12 }.run(&episode).unwrap();
    assert_eq!(lenient.verdict, Verdict::Passed);
    assert_eq!(
        lenient.message,
        "Kapto dropped 15 messages, no more than the 12 allowed per stream"
    );
    let strict = StreamsComplete { max_dropped: 5 }.run(&episode).unwrap();
    assert_eq!(strict.verdict, Verdict::Failed);
    assert_eq!(
        strict.message,
        "Kapto dropped 15 messages: arm_joints (12 of 2000 received), more than the 5 allowed per stream"
    );
}

#[test]
fn the_settings_make_each_check_they_turn_on_with_its_threshold() {
    let ids = |settings: &QaSettings| {
        from_settings(settings)
            .iter()
            .map(|check| check.id())
            .collect::<Vec<_>>()
    };
    assert_eq!(
        ids(&QaSettings::default()),
        [
            "streams.complete",
            "streams.present",
            "streams.empty",
            "episode.short",
            "episode.long",
            "streams.timing",
            "streams.rate",
            "streams.gap",
            "camera.format",
            "camera.frames",
            "motion.jump",
            "motion.frozen",
            "motion.chatter",
            "motion.idle"
        ]
    );
    let off = QaSettings {
        params_version: Some("2".into()),
        streams_complete: StreamsCompleteSettings {
            enabled: false,
            max_dropped: 0,
        },
        joint_jumps: JointJumpsSettings {
            enabled: false,
            ..JointJumpsSettings::default()
        },
        frozen_joints: FrozenJointsSettings {
            enabled: false,
            ..FrozenJointsSettings::default()
        },
        gripper_chatter: GripperChatterSettings {
            enabled: false,
            ..GripperChatterSettings::default()
        },
        idle: IdleSettings {
            enabled: false,
            ..IdleSettings::default()
        },
        streams_present: StreamsPresentSettings {
            enabled: false,
            empty_enabled: false,
            ..StreamsPresentSettings::default()
        },
        duration: DurationSettings {
            short_enabled: false,
            long_enabled: false,
            ..DurationSettings::default()
        },
        cameras: CameraSettings {
            format_enabled: false,
            frames_enabled: false,
            ..CameraSettings::default()
        },
        stream_timing: StreamTimingSettings {
            timing_enabled: false,
            rate_enabled: false,
            gap_enabled: false,
            ..StreamTimingSettings::default()
        },
        ..QaSettings::default()
    };
    assert_eq!(ids(&off), Vec::<&str>::new());
    let lenient = QaSettings {
        params_version: Some("3".into()),
        streams_complete: StreamsCompleteSettings {
            enabled: true,
            max_dropped: 20,
        },
        ..QaSettings::default()
    };
    let checks = from_settings(&lenient);
    let finding = checks[0]
        .run(&episode(vec![stream("arm_joints", 12)]))
        .unwrap();
    assert_eq!(finding.verdict, Verdict::Passed);
}
