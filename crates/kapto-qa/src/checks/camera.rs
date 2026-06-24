// SPDX-License-Identifier: Apache-2.0

//! Checks of each camera's frames, as read with the episode's roles.

use kapto_core::KaptoError;
use kapto_store::TimeRange;

use crate::cameras::CameraFrames;
use crate::{Check, CheckKind, EpisodeView, Finding};

/// Each camera's first frame holds a picture of the camera's kind: a gate,
/// as a camera whose first frame doesn't is one declared wrongly (a raw
/// camera sending JPEG, a size that isn't its own), and its pictures can't
/// be used.
#[derive(Clone, Copy, Debug, Default)]
pub struct CameraFormat;

/// Each camera's later frames hold a picture, and none is much smaller
/// than its usual: advisory, as a frame far smaller than the median is
/// most often black, blank or cut short, and one without a picture was
/// broken on the way.
#[derive(Clone, Copy, Debug)]
pub struct CameraFrameSizes {
    /// The share of the median size below which a frame is flagged.
    pub min_size_ratio: f64,
}

/// The fewest pictures a camera needs before its median says what usual
/// is.
const MIN_FRAMES: usize = 10;

/// The most ranges a finding names per camera.
const MAX_RANGES: usize = 20;

impl Check for CameraFormat {
    fn id(&self) -> &'static str {
        "camera.format"
    }

    fn version(&self) -> u32 {
        1
    }

    fn kind(&self) -> CheckKind {
        CheckKind::Gate
    }

    fn run(&self, episode: &EpisodeView) -> Result<Finding, KaptoError> {
        let cameras = episode.cameras();
        if cameras.is_empty() {
            return Ok(Finding::not_applicable("the episode has no camera"));
        }
        let wrong: Vec<String> = cameras
            .iter()
            .filter_map(|(name, frames)| {
                let why = frames.first_unreadable.as_ref()?;
                Some(format!("{name}'s first frame holds {why}"))
            })
            .collect();
        // A count of cameras, far below 2^53.
        #[allow(clippy::cast_precision_loss, reason = "a count of cameras")]
        let finding = if wrong.is_empty() {
            Finding::passed("each camera's first frame holds a picture of its kind")
        } else {
            Finding::failed(wrong.join("; "))
        }
        .with_value("wrong", wrong.len() as f64);
        Ok(finding)
    }
}

impl Check for CameraFrameSizes {
    fn id(&self) -> &'static str {
        "camera.frames"
    }

    fn version(&self) -> u32 {
        1
    }

    fn kind(&self) -> CheckKind {
        CheckKind::Advisory
    }

    fn run(&self, episode: &EpisodeView) -> Result<Finding, KaptoError> {
        let cameras = episode.cameras();
        if cameras.is_empty() {
            return Ok(Finding::not_applicable("the episode has no camera"));
        }
        let mut problems = Vec::new();
        let mut finding_values = Vec::new();
        let mut ranges = Vec::new();
        for (name, frames) in cameras {
            let median = median(&frames.sizes);
            let small = small_frames(frames, median, self.min_size_ratio);
            let unreadable = frames.unreadable_utc_ns.len();
            if unreadable > 0 {
                let why = frames.unreadable_why.as_deref().unwrap_or("no picture");
                problems.push(format!(
                    "{name}: {unreadable} of {} frames hold no picture ({why})",
                    frames.frames()
                ));
            }
            if !small.is_empty() {
                problems.push(format!(
                    "{name}: {} frames under {:.0} % of its median size, {} bytes",
                    small.len(),
                    self.min_size_ratio * 100.0,
                    median.unwrap_or(0)
                ));
            }
            let mut flagged: Vec<i64> = small
                .iter()
                .chain(&frames.unreadable_utc_ns)
                .copied()
                .collect();
            flagged.sort_unstable();
            ranges.extend(runs(frames, &flagged).into_iter().take(MAX_RANGES));
            finding_values.push((format!("{name}.unreadable"), unreadable));
            finding_values.push((format!("{name}.small"), small.len()));
        }
        let finding = if problems.is_empty() {
            Finding::passed("every frame holds a picture of a usual size")
        } else {
            Finding::failed(problems.join("; "))
        };
        // Counts of frames, far below 2^53.
        #[allow(clippy::cast_precision_loss, reason = "counts of frames")]
        let finding = finding_values
            .into_iter()
            .fold(finding, |finding, (name, count)| {
                finding.with_value(&name, count as f64)
            });
        Ok(ranges.into_iter().fold(finding, Finding::with_range))
    }
}

/// The median of `sizes`, once there are enough to say what usual is.
fn median(sizes: &[u64]) -> Option<u64> {
    if sizes.len() < MIN_FRAMES {
        return None;
    }
    let mut sorted = sizes.to_vec();
    let middle = sorted.len() / 2;
    Some(*sorted.select_nth_unstable(middle).1)
}

/// When each of `frames`' pictures under `ratio` of `median` was received.
fn small_frames(frames: &CameraFrames, median: Option<u64>, ratio: f64) -> Vec<i64> {
    let Some(median) = median else {
        return Vec::new();
    };
    // Sizes of pictures, far below 2^53.
    #[allow(clippy::cast_precision_loss, reason = "sizes of pictures")]
    let least = median as f64 * ratio;
    frames
        .times_utc_ns
        .iter()
        .zip(&frames.sizes)
        .filter(|&(_, &size)| {
            #[allow(clippy::cast_precision_loss, reason = "sizes of pictures")]
            let size = size as f64;
            size < least
        })
        .map(|(&time, _)| time)
        .collect()
}

/// The runs of consecutive frames among `flagged`, each as the span from
/// its first to its last: a frame of the camera received between two
/// flagged ones ends a run.
fn runs(frames: &CameraFrames, flagged: &[i64]) -> Vec<TimeRange> {
    let mut all: Vec<i64> = frames
        .times_utc_ns
        .iter()
        .chain(&frames.unreadable_utc_ns)
        .copied()
        .collect();
    all.sort_unstable();
    let mut ranges: Vec<TimeRange> = Vec::new();
    let mut previous: Option<usize> = None;
    for &time in flagged {
        let at = all.partition_point(|&other| other < time);
        match (ranges.last_mut(), previous) {
            (Some(last), Some(before)) if at == before + 1 => last.end_utc_ns = time,
            _ => ranges.push(TimeRange {
                start_utc_ns: time,
                end_utc_ns: time,
            }),
        }
        previous = Some(at);
    }
    ranges
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use kapto_store::Verdict;
    use kapto_testkit::stream;

    use super::*;

    const START: i64 = 1_772_700_400_000_000_000;

    fn episode(cameras: Vec<(&str, CameraFrames)>) -> EpisodeView {
        let summary = kapto_testkit::episode(
            "20260305T192345.123Z-1a2b3c4d",
            (START, START + 10_000_000_000),
            vec![stream("wrist", 30)],
        );
        EpisodeView::new(std::path::Path::new("."), summary).with_cameras(
            cameras
                .into_iter()
                .map(|(name, frames)| (name.to_owned(), frames))
                .collect::<BTreeMap<_, _>>(),
        )
    }

    /// A camera's frames a tenth of a second apart, of `sizes`.
    fn frames(sizes: &[u64]) -> CameraFrames {
        CameraFrames {
            times_utc_ns: (0..)
                .map(|n| START + n * 100_000_000)
                .take(sizes.len())
                .collect(),
            sizes: sizes.to_vec(),
            ..CameraFrames::default()
        }
    }

    #[test]
    fn a_camera_whose_first_frame_holds_no_picture_of_its_kind_fails_the_episode() {
        let good = episode(vec![("wrist", frames(&[900; 3]))]);
        assert_eq!(CameraFormat.run(&good).unwrap().verdict, Verdict::Passed);
        let declared_wrongly = CameraFrames {
            first_unreadable: Some("pictures in `jpeg`, neither JPEG nor PNG".to_owned()),
            ..frames(&[900; 3])
        };
        let finding = CameraFormat
            .run(&episode(vec![("wrist", declared_wrongly)]))
            .unwrap();
        assert_eq!(finding.verdict, Verdict::Failed);
        assert_eq!(
            finding.message,
            "wrist's first frame holds pictures in `jpeg`, neither JPEG nor PNG"
        );
        assert_eq!(
            CameraFormat.run(&episode(Vec::new())).unwrap().verdict,
            Verdict::NotApplicable
        );
    }

    #[test]
    fn frames_far_smaller_than_the_median_are_flagged_where_they_are() {
        let check = CameraFrameSizes {
            min_size_ratio: 0.3,
        };
        let usual = episode(vec![("wrist", frames(&[1000; 20]))]);
        assert_eq!(check.run(&usual).unwrap().verdict, Verdict::Passed);
        // Two black frames in a row, then one more later.
        let mut sizes = vec![1000; 20];
        sizes[5] = 120;
        sizes[6] = 100;
        sizes[12] = 299;
        let finding = check
            .run(&episode(vec![("wrist", frames(&sizes))]))
            .unwrap();
        assert_eq!(finding.verdict, Verdict::Failed);
        assert_eq!(
            finding.message,
            "wrist: 3 frames under 30 % of its median size, 1000 bytes"
        );
        assert_eq!(finding.values["wrist.small"], 3.0);
        let at = |n: i64| START + n * 100_000_000;
        assert_eq!(
            finding.ranges,
            [
                TimeRange {
                    start_utc_ns: at(5),
                    end_utc_ns: at(6)
                },
                TimeRange {
                    start_utc_ns: at(12),
                    end_utc_ns: at(12)
                }
            ]
        );
        // Too few to say what usual is.
        let few = episode(vec![("wrist", frames(&[1000, 10, 1000]))]);
        assert_eq!(check.run(&few).unwrap().verdict, Verdict::Passed);
    }

    #[test]
    fn a_later_frame_without_a_picture_is_flagged_but_does_not_reject_the_episode() {
        let broken = CameraFrames {
            unreadable_utc_ns: vec![START + 50],
            unreadable_why: Some("a frame that can't be read: cut short".to_owned()),
            ..frames(&[1000; 12])
        };
        let episode = episode(vec![("wrist", broken)]);
        let finding = CameraFrameSizes {
            min_size_ratio: 0.3,
        }
        .run(&episode)
        .unwrap();
        assert_eq!(
            finding.message,
            "wrist: 1 of 13 frames hold no picture (a frame that can't be read: cut short)"
        );
        assert_eq!(CameraFormat.run(&episode).unwrap().verdict, Verdict::Passed);
    }
}
