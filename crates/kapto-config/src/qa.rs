// SPDX-License-Identifier: Apache-2.0

//! Which quality checks run on the episodes recorded with a robot package,
//! and their thresholds, from the package's `qa.toml`. Each check's
//! settings are in the table named after its ID:
//!
//! ```toml
//! qa_params_version = "1"
//!
//! [streams.complete]
//! max_dropped = 0
//!
//! [anomalies.statistics]
//! threshold = 3.5
//! ```
//!
//! What finds anomalies is set the same way, in the table named after its
//! ID. A package without `qa.toml` runs every check with its defaults. Each
//! result records `qa_params_version`, so results computed with different
//! thresholds aren't taken for alike.

use kapto_core::{ErrorCode, KaptoError};
use toml_edit::{Document, TableLike};

use crate::file::ConfigFile;
use crate::located::{at_key, syntax_error, unknown_key};
use crate::reader::{FileKey, Reader};
use crate::suggest::closest;

/// The name of the quality settings' file in a robot package.
pub const QA_FILE: &str = "qa.toml";

/// Every key of `qa.toml`, in the order they are documented: a check's
/// settings are named after its ID.
pub const QA_KEYS: [FileKey; 30] = [
    FileKey {
        name: "qa_params_version",
        required: true,
        help: "The version of these settings, text without spaces, e.g. `3`. Recorded with every result; change it whenever a setting changes.",
    },
    FileKey {
        name: "statistics.exclude",
        required: false,
        help: "The statistics not kept, shown or compared by any detector: a list of names, where `*` stands for any part of one name between dots, e.g. `[\"streams.*.lost_upstream_ratio\", \"joints.wrist_*.jerk\"]`. Every statistic is on by default, those added in later versions too.",
    },
    FileKey {
        name: "statistics.include",
        required: false,
        help: "Statistics turned back on among those `statistics.exclude` turns off, written the same way, e.g. `[\"streams.camera.lost_upstream_ratio\"]`.",
    },
    FileKey {
        name: "streams.complete.enabled",
        required: false,
        help: "Whether the gate `streams.complete` runs: every message that reached Kapto was written. `true` by default.",
    },
    FileKey {
        name: "streams.complete.max_dropped",
        required: false,
        help: "The most messages Kapto may drop from one stream before `streams.complete` fails the episode, from 0. `0` by default.",
    },
    FileKey {
        name: "motion.jump.enabled",
        required: false,
        help: "Whether `motion.jump` runs: it flags each moment a joint moved faster than `max_speed` between two messages, as a reading that jumped does. Advisory. `true` by default.",
    },
    FileKey {
        name: "motion.jump.max_speed",
        required: false,
        help: "The fastest a joint can move, in rad/s for a turning joint or m/s for a sliding one, from 0.001 to 1000. `20` by default: faster than any arm's motors.",
    },
    FileKey {
        name: "motion.frozen.enabled",
        required: false,
        help: "Whether `motion.frozen` runs: it flags each time a joint's reading stayed exactly the same for `max_seconds` right after it moved at `moving_speed`, while another joint moved, as a reading that stopped updating does. Advisory. `true` by default.",
    },
    FileKey {
        name: "motion.frozen.max_seconds",
        required: false,
        help: "The shortest stillness flagged, in seconds, from 0.001 to 3600. `1` by default.",
    },
    FileKey {
        name: "motion.frozen.moving_speed",
        required: false,
        help: "How fast a joint must have been moving just before, in rad/s or m/s, from 0 to 1000: a joint slowing down to rest gets slower than this first. `0.05` by default.",
    },
    FileKey {
        name: "motion.chatter.enabled",
        required: false,
        help: "Whether `motion.chatter` runs: it flags a gripper (each role ending in `opening`) that closed more than `max_closings` times within `window_seconds`. Advisory. `true` by default.",
    },
    FileKey {
        name: "motion.chatter.max_closings",
        required: false,
        help: "The most closings allowed within the window, from 1 to 1000. `3` by default.",
    },
    FileKey {
        name: "motion.chatter.window_seconds",
        required: false,
        help: "The window, in seconds, from 0.001 to 3600. `2` by default.",
    },
    FileKey {
        name: "motion.idle.enabled",
        required: false,
        help: "Whether `motion.idle` runs: it flags an episode in which the robot stood still, every joint slower than `moving_speed`, for more than `max_ratio` of it, counting spans of `min_seconds` or more. Advisory. `true` by default.",
    },
    FileKey {
        name: "motion.idle.max_ratio",
        required: false,
        help: "The largest share of the episode the robot may stand still, from 0 to 1. `0.5` by default.",
    },
    FileKey {
        name: "motion.idle.min_seconds",
        required: false,
        help: "The shortest still span counted, in seconds, from 0.001 to 3600: shorter pauses are part of the task. `2` by default.",
    },
    FileKey {
        name: "motion.idle.moving_speed",
        required: false,
        help: "Slower than this, in rad/s or m/s, a joint counts as still, from 0 to 1000. `0.05` by default.",
    },
    FileKey {
        name: "anomalies.statistics.enabled",
        required: false,
        help: "Whether `anomalies.statistics` runs: it flags an episode whose statistics are far from those of the other episodes of its session (the same robot package, profile and task, on the same day). It only flags, never rejects. `true` by default.",
    },
    FileKey {
        name: "anomalies.statistics.threshold",
        required: false,
        help: "How far from the session's median a statistic must be to be flagged, in robust standard deviations (1.4826 times the median absolute deviation), from 1 to 100. `3.5` by default.",
    },
    FileKey {
        name: "anomalies.statistics.min_episodes",
        required: false,
        help: "How many other checked episodes the session needs before an episode is compared with them, from 1 to 1000. `5` by default.",
    },
    FileKey {
        name: "anomalies.statistics.max_episodes",
        required: false,
        help: "How many of the session's other episodes an episode is compared with at most, the newest, from 1 to 10000. `200` by default.",
    },
    FileKey {
        name: "anomalies.autoencoder.enabled",
        required: false,
        help: "Whether `anomalies.autoencoder` runs: it learns which statistics go together from the accepted episodes of the robot package and task, and flags an episode whose mix of statistics it can't reconstruct. It only flags, never rejects; until it has learned from `min_episodes` episodes it says how many more it needs. `true` by default.",
    },
    FileKey {
        name: "anomalies.autoencoder.model",
        required: false,
        help: "Which autoencoder: `\"linear\"`, principal components, which learn straight-line relations in milliseconds; or `\"neural\"`, a small neural network, which learns curved ones too (effort growing with the square of speed) in seconds. `\"linear\"` by default.",
    },
    FileKey {
        name: "anomalies.autoencoder.neurons",
        required: false,
        help: "How many neurons each of the neural autoencoder's two hidden layers has, from 1 to 256: more learn more detail and take longer. Read only for `model = \"neural\"`. `16` by default.",
    },
    FileKey {
        name: "anomalies.autoencoder.train_seconds",
        required: false,
        help: "How long learning a model may take, in seconds, from 0.001 to 3600; if it takes longer it stops, the previous model stays in use and each result says so. `60` by default.",
    },
    FileKey {
        name: "anomalies.autoencoder.threshold",
        required: false,
        help: "How far beyond the usual reconstruction error an episode must be to be flagged, in robust standard deviations of the errors of the episodes learned from, from 1 to 100. `3.5` by default.",
    },
    FileKey {
        name: "anomalies.autoencoder.min_episodes",
        required: false,
        help: "How many accepted episodes of the package and task are needed before a model is learned, from 3 to 10000. `20` by default.",
    },
    FileKey {
        name: "anomalies.autoencoder.max_episodes",
        required: false,
        help: "How many of them, the newest, a model learns from at most, from 3 to 10000. `200` by default.",
    },
    FileKey {
        name: "anomalies.autoencoder.variance",
        required: false,
        help: "How much of the spread of the statistics the model keeps, from 0.5 to 1: the more, the closer it follows the episodes learned from. `0.9` by default.",
    },
    FileKey {
        name: "anomalies.autoencoder.retrain_after",
        required: false,
        help: "How many new accepted episodes of the package and task make it learn again, from 1 to 10000. `10` by default.",
    },
];

/// The quality settings of a robot package.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct QaSettings {
    /// `qa_params_version`; absent for a package without `qa.toml`.
    pub params_version: Option<String>,
    /// Which statistics are worked out.
    pub statistics: StatisticsSelection,
    /// The gate `streams.complete`.
    pub streams_complete: StreamsCompleteSettings,
    /// The advisory `motion.jump`.
    pub joint_jumps: JointJumpsSettings,
    /// The advisory `motion.frozen`.
    pub frozen_joints: FrozenJointsSettings,
    /// The advisory `motion.chatter`.
    pub gripper_chatter: GripperChatterSettings,
    /// The advisory `motion.idle`.
    pub idle: IdleSettings,
    /// The anomalies found by comparing statistics, `anomalies.statistics`.
    pub statistical_anomalies: StatisticalAnomaliesSettings,
    /// The anomalies an autoencoder finds, `anomalies.autoencoder`.
    pub autoencoder_anomalies: AutoencoderAnomaliesSettings,
}

/// The settings of the gate `streams.complete`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StreamsCompleteSettings {
    /// Whether it runs.
    pub enabled: bool,
    /// The most messages Kapto may drop from one stream.
    pub max_dropped: u64,
}

impl Default for StreamsCompleteSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            max_dropped: 0,
        }
    }
}

/// The settings of the advisory `motion.jump`.
#[derive(Clone, Debug, PartialEq)]
pub struct JointJumpsSettings {
    /// Whether it runs.
    pub enabled: bool,
    /// The fastest a joint can move, in rad/s or m/s.
    pub max_speed: f64,
}

impl Default for JointJumpsSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            // Faster than any arm's motors: only a reading that jumped.
            max_speed: 20.0,
        }
    }
}

/// The settings of the advisory `motion.frozen`.
#[derive(Clone, Debug, PartialEq)]
pub struct FrozenJointsSettings {
    /// Whether it runs.
    pub enabled: bool,
    /// The shortest stillness flagged, in seconds.
    pub max_seconds: f64,
    /// How fast a joint must have been moving just before, in rad/s or m/s.
    pub moving_speed: f64,
}

impl Default for FrozenJointsSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            max_seconds: 1.0,
            moving_speed: 0.05,
        }
    }
}

/// The settings of the advisory `motion.chatter`.
#[derive(Clone, Debug, PartialEq)]
pub struct GripperChatterSettings {
    /// Whether it runs.
    pub enabled: bool,
    /// The most closings allowed within the window.
    pub max_closings: usize,
    /// The window, in seconds.
    pub window_seconds: f64,
}

impl Default for GripperChatterSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            max_closings: 3,
            window_seconds: 2.0,
        }
    }
}

/// The settings of the advisory `motion.idle`.
#[derive(Clone, Debug, PartialEq)]
pub struct IdleSettings {
    /// Whether it runs.
    pub enabled: bool,
    /// The largest share of the episode still.
    pub max_ratio: f64,
    /// The shortest still span counted, in seconds.
    pub min_seconds: f64,
    /// Slower than this, in rad/s or m/s, a joint is still.
    pub moving_speed: f64,
}

impl Default for IdleSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            max_ratio: 0.5,
            min_seconds: 2.0,
            moving_speed: 0.05,
        }
    }
}

/// Which statistics are worked out, kept and compared: all but those
/// `exclude` names, unless `include` names them too.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct StatisticsSelection {
    /// Patterns of the statistics turned off.
    pub exclude: Vec<String>,
    /// Patterns of those turned back on.
    pub include: Vec<String>,
}

impl StatisticsSelection {
    /// Whether the statistic `name` is kept.
    #[must_use]
    pub fn keeps(&self, name: &str) -> bool {
        let named = |patterns: &[String]| patterns.iter().any(|pattern| matches(pattern, name));
        !named(&self.exclude) || named(&self.include)
    }

    /// The patterns that name none of `names`, which are likely mistyped.
    pub fn unmatched<'a>(&'a self, names: &[&str]) -> impl Iterator<Item = &'a str> {
        self.exclude
            .iter()
            .chain(&self.include)
            .filter(move |pattern| !names.iter().any(|name| matches(pattern, name)))
            .map(String::as_str)
    }
}

/// Whether `name` matches `pattern`: the same number of names between dots,
/// each equal, or matching where the pattern's name has `*`, which stands
/// for any part of one name.
fn matches(pattern: &str, name: &str) -> bool {
    let (patterns, names): (Vec<&str>, Vec<&str>) =
        (pattern.split('.').collect(), name.split('.').collect());
    patterns.len() == names.len()
        && patterns
            .iter()
            .zip(&names)
            .all(|(pattern, name)| part_matches(pattern, name))
}

/// Whether one name matches one pattern name, `*` standing for any run of
/// characters.
fn part_matches(pattern: &str, name: &str) -> bool {
    match pattern.split_once('*') {
        None => pattern == name,
        Some((before, after)) => {
            name.len() >= before.len()
                && name.starts_with(before)
                && (0..=name.len() - before.len()).any(|skip| {
                    name.get(before.len() + skip..)
                        .is_some_and(|rest| part_matches(after, rest))
                })
        }
    }
}

/// A statistic pattern: names between dots, none empty, without spaces.
fn is_pattern(text: &str) -> bool {
    !text.split('.').any(str::is_empty) && !text.contains(char::is_whitespace)
}

/// The settings of `anomalies.statistics`.
#[derive(Clone, Debug, PartialEq)]
pub struct StatisticalAnomaliesSettings {
    /// Whether it runs.
    pub enabled: bool,
    /// How far from the session's median a statistic is flagged, in robust
    /// standard deviations.
    pub threshold: f64,
    /// The fewest other episodes it compares an episode with.
    pub min_episodes: usize,
    /// The most other episodes it compares an episode with, the newest.
    pub max_episodes: usize,
}

impl Default for StatisticalAnomaliesSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            // A common cut for robust z-scores: rare for values that are
            // alike, whatever their distribution.
            threshold: 3.5,
            min_episodes: 5,
            max_episodes: 200,
        }
    }
}

/// Which autoencoder learns a package and task's episodes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum AutoencoderChoice {
    /// Principal components: straight-line relations, in milliseconds.
    #[default]
    Linear,
    /// A small neural network: curved relations too, in seconds.
    Neural,
}

impl AutoencoderChoice {
    /// Its name in `qa.toml`.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::Linear => "linear",
            Self::Neural => "neural",
        }
    }
}

/// The settings of `anomalies.autoencoder`.
#[derive(Clone, Debug, PartialEq)]
pub struct AutoencoderAnomaliesSettings {
    /// Whether it runs.
    pub enabled: bool,
    /// Which autoencoder.
    pub model: AutoencoderChoice,
    /// Neurons in each hidden layer of the neural autoencoder.
    pub neurons: usize,
    /// How long learning may take, in seconds.
    pub train_seconds: f64,
    /// How far beyond the usual reconstruction error is flagged, in robust
    /// standard deviations.
    pub threshold: f64,
    /// The fewest accepted episodes a model is learned from.
    pub min_episodes: usize,
    /// The most, the newest.
    pub max_episodes: usize,
    /// How much of the statistics' spread a model keeps, from 0.5 to 1.
    pub variance: f64,
    /// How many new accepted episodes make it learn again.
    pub retrain_after: usize,
}

impl Default for AutoencoderAnomaliesSettings {
    fn default() -> Self {
        Self {
            // On: learning takes milliseconds to seconds every few
            // episodes, and until it has enough it says so.
            enabled: true,
            model: AutoencoderChoice::Linear,
            neurons: 16,
            // Learning the neural model from 200 episodes of 300 statistics
            // takes about 11 s on a laptop: room for a slower machine.
            train_seconds: 60.0,
            threshold: 3.5,
            min_episodes: 20,
            max_episodes: 200,
            variance: 0.9,
            retrain_after: 10,
        }
    }
}

/// Reads the quality settings in `file`; what it leaves out keeps its
/// default.
///
/// # Errors
///
/// Every problem found, each with the file and line: invalid TOML
/// (`config.invalid_syntax`), keys that aren't a setting (`qa.unknown_key`),
/// and missing or invalid values (`config.invalid_value`).
pub fn parse_qa(file: &ConfigFile) -> Result<QaSettings, Vec<KaptoError>> {
    let document = Document::parse(file.contents.as_str())
        .map_err(|error| vec![syntax_error(file, &error)])?;
    let root = document.as_table();
    let mut errors = Vec::new();
    unknown_keys(file, root, "", &mut errors);

    let mut reader = Reader::new(file, root);
    let params_version = reader.text(
        "qa_params_version",
        "text without spaces, e.g. \"3\"",
        |text| !text.is_empty() && !text.contains(char::is_whitespace),
    );
    if root.get("qa_params_version").is_none() {
        let first = root
            .iter()
            .next()
            .map_or("qa_params_version", |(key, _)| key);
        let error = KaptoError::new(ErrorCode::ConfigInvalidValue)
            .detail(format!("`{QA_FILE}` needs `qa_params_version`"));
        reader.located(error, first);
    }
    errors.append(&mut reader.errors);

    let mut settings = QaSettings {
        params_version,
        ..QaSettings::default()
    };
    read_settings(file, root, &mut settings, &mut errors);
    if errors.is_empty() {
        Ok(settings)
    } else {
        Err(errors)
    }
}

/// Reads the settings of `anomalies.autoencoder` from its `table` into
/// `detector`, keeping each one left out.
fn read_autoencoder(
    reader: &mut Reader<'_>,
    table: &dyn TableLike,
    detector: &mut AutoencoderAnomaliesSettings,
) {
    detector.enabled = reader.switch("enabled").unwrap_or(detector.enabled);
    detector.model = match reader
        .text("model", "\"linear\" or \"neural\"", |text| {
            matches!(text, "linear" | "neural")
        })
        .as_deref()
    {
        Some("neural") => AutoencoderChoice::Neural,
        Some(_) => AutoencoderChoice::Linear,
        None => detector.model,
    };
    detector.neurons = reader
        .whole("neurons", 1..=256)
        .and_then(|whole| usize::try_from(whole).ok())
        .unwrap_or(detector.neurons);
    detector.train_seconds = reader
        .number("train_seconds", 0.001..=3600.0)
        .unwrap_or(detector.train_seconds);
    detector.threshold = reader
        .number("threshold", 1.0..=100.0)
        .unwrap_or(detector.threshold);
    detector.variance = reader
        .number("variance", 0.5..=1.0)
        .unwrap_or(detector.variance);
    let mut count = |key, least| {
        reader
            .whole(key, least..=10_000)
            .and_then(|whole| usize::try_from(whole).ok())
    };
    detector.min_episodes = count("min_episodes", 3).unwrap_or(detector.min_episodes);
    detector.max_episodes = count("max_episodes", 3).unwrap_or(detector.max_episodes);
    detector.retrain_after = count("retrain_after", 1).unwrap_or(detector.retrain_after);
    min_above_max(
        reader,
        table,
        (detector.min_episodes, detector.max_episodes),
        "no model would ever be learned",
    );
}

/// Reads each check's, detector's and the statistics' tables below `root`
/// into `settings`, adding their problems to `errors`.
fn read_settings(
    file: &ConfigFile,
    root: &dyn TableLike,
    settings: &mut QaSettings,
    errors: &mut Vec<KaptoError>,
) {
    read_table(file, root, "statistics", errors, |reader, _| {
        let expected = "statistic names, `*` for any part of one, e.g. \"joints.*.jerk\"";
        settings.statistics = StatisticsSelection {
            exclude: reader.texts("exclude", expected, is_pattern),
            include: reader.texts("include", expected, is_pattern),
        };
    });
    read_table(file, root, "streams.complete", errors, |reader, _| {
        let check = &mut settings.streams_complete;
        check.enabled = reader.switch("enabled").unwrap_or(check.enabled);
        check.max_dropped = reader
            .whole("max_dropped", 0..=i64::MAX)
            .and_then(|whole| u64::try_from(whole).ok())
            .unwrap_or(check.max_dropped);
    });
    read_table(file, root, "motion.jump", errors, |reader, _| {
        let check = &mut settings.joint_jumps;
        check.enabled = reader.switch("enabled").unwrap_or(check.enabled);
        check.max_speed = reader
            .number("max_speed", 0.001..=1000.0)
            .unwrap_or(check.max_speed);
    });
    read_table(file, root, "motion.frozen", errors, |reader, _| {
        let check = &mut settings.frozen_joints;
        check.enabled = reader.switch("enabled").unwrap_or(check.enabled);
        check.max_seconds = reader
            .number("max_seconds", 0.001..=3600.0)
            .unwrap_or(check.max_seconds);
        check.moving_speed = reader
            .number("moving_speed", 0.0..=1000.0)
            .unwrap_or(check.moving_speed);
    });
    read_table(file, root, "motion.chatter", errors, |reader, _| {
        let check = &mut settings.gripper_chatter;
        check.enabled = reader.switch("enabled").unwrap_or(check.enabled);
        check.max_closings = reader
            .whole("max_closings", 1..=1000)
            .and_then(|whole| usize::try_from(whole).ok())
            .unwrap_or(check.max_closings);
        check.window_seconds = reader
            .number("window_seconds", 0.001..=3600.0)
            .unwrap_or(check.window_seconds);
    });
    read_table(file, root, "motion.idle", errors, |reader, _| {
        let check = &mut settings.idle;
        check.enabled = reader.switch("enabled").unwrap_or(check.enabled);
        check.max_ratio = reader
            .number("max_ratio", 0.0..=1.0)
            .unwrap_or(check.max_ratio);
        check.min_seconds = reader
            .number("min_seconds", 0.001..=3600.0)
            .unwrap_or(check.min_seconds);
        check.moving_speed = reader
            .number("moving_speed", 0.0..=1000.0)
            .unwrap_or(check.moving_speed);
    });
    read_table(
        file,
        root,
        "anomalies.statistics",
        errors,
        |reader, table| {
            let detector = &mut settings.statistical_anomalies;
            detector.enabled = reader.switch("enabled").unwrap_or(detector.enabled);
            detector.threshold = reader
                .number("threshold", 1.0..=100.0)
                .unwrap_or(detector.threshold);
            let mut count = |key, most| {
                reader
                    .whole(key, 1..=most)
                    .and_then(|whole| usize::try_from(whole).ok())
            };
            detector.min_episodes = count("min_episodes", 1000).unwrap_or(detector.min_episodes);
            detector.max_episodes = count("max_episodes", 10_000).unwrap_or(detector.max_episodes);
            min_above_max(
                reader,
                table,
                (detector.min_episodes, detector.max_episodes),
                "no episode would ever be compared",
            );
        },
    );
    read_table(
        file,
        root,
        "anomalies.autoencoder",
        errors,
        |reader, table| {
            read_autoencoder(reader, table, &mut settings.autoencoder_anomalies);
        },
    );
}

/// Reads the table at the dotted `path` below `root`, if there is one,
/// with `read`, adding its problems to `errors`.
fn read_table(
    file: &ConfigFile,
    root: &dyn TableLike,
    path: &str,
    errors: &mut Vec<KaptoError>,
    read: impl FnOnce(&mut Reader<'_>, &dyn TableLike),
) {
    if let Some(table) = table_at(root, path) {
        let mut reader = Reader::new(file, table);
        read(&mut reader, table);
        errors.append(&mut reader.errors);
    }
}

/// Reports `min_episodes` above `max_episodes` in `table`, saying what
/// that would mean, at the one written if only one is.
fn min_above_max(
    reader: &mut Reader<'_>,
    table: &dyn TableLike,
    (min, max): (usize, usize),
    meaning: &str,
) {
    if min > max {
        let error = KaptoError::new(ErrorCode::ConfigInvalidValue).detail(format!(
            "`min_episodes` ({min}) is more than `max_episodes` ({max}): {meaning}"
        ));
        let key = if table.contains_key("min_episodes") {
            "min_episodes"
        } else {
            "max_episodes"
        };
        reader.located(error, key);
    }
}

/// Reports each key of `table`, the table at `prefix`, that is neither a
/// setting nor a table leading to one, suggesting the closest setting or
/// table, in the order they are in the file.
fn unknown_keys(
    file: &ConfigFile,
    table: &dyn TableLike,
    prefix: &str,
    errors: &mut Vec<KaptoError>,
) {
    for (key, item) in table.iter() {
        let path = if prefix.is_empty() {
            key.to_owned()
        } else {
            format!("{prefix}.{key}")
        };
        let setting = QA_KEYS.iter().any(|known| known.name == path);
        let leads = QA_KEYS
            .iter()
            .any(|known| known.name.starts_with(&format!("{path}.")));
        if leads && let Some(inner) = item.as_table_like() {
            unknown_keys(file, inner, &path, errors);
        } else if !setting {
            // Every setting and every table on the way to one.
            let names = QA_KEYS.iter().flat_map(|known| {
                known
                    .name
                    .match_indices('.')
                    .map(|(dot, _)| &known.name[..dot])
                    .chain([known.name])
            });
            // A setting written outside its check's table is found by its
            // own name.
            let moved = QA_KEYS
                .iter()
                .map(|known| known.name)
                .find(|known| known.ends_with(&format!(".{key}")));
            let suggestion = moved.or_else(|| closest(&path, names));
            let error = unknown_key(ErrorCode::QaUnknownKey, &path, suggestion);
            errors.push(at_key(error, file, table, key));
        }
    }
}

/// The table at the dotted `path` below `root`, if there is one.
fn table_at<'a>(root: &'a dyn TableLike, path: &str) -> Option<&'a dyn TableLike> {
    path.split('.')
        .try_fold(root, |table, key| table.get(key)?.as_table_like())
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    fn parse(contents: &str) -> Result<QaSettings, Vec<KaptoError>> {
        parse_qa(&ConfigFile {
            path: PathBuf::from("/packages/arm7/qa.toml"),
            contents: contents.to_owned(),
        })
    }

    fn problems(contents: &str) -> Vec<String> {
        parse(contents)
            .unwrap_err()
            .iter()
            .map(|error| {
                let line = error
                    .context_entries()
                    .iter()
                    .find(|(key, _)| *key == "line")
                    .map(|(_, line)| line.clone())
                    .unwrap_or_default();
                format!("{} {line}: {}", error.code().id(), error.message())
            })
            .collect()
    }

    #[test]
    fn settings_left_out_keep_their_defaults() {
        assert_eq!(
            parse("qa_params_version = \"1\"\n").unwrap(),
            QaSettings {
                params_version: Some("1".into()),
                statistics: StatisticsSelection::default(),
                streams_complete: StreamsCompleteSettings {
                    enabled: true,
                    max_dropped: 0,
                },
                joint_jumps: JointJumpsSettings {
                    enabled: true,
                    max_speed: 20.0,
                },
                frozen_joints: FrozenJointsSettings {
                    enabled: true,
                    max_seconds: 1.0,
                    moving_speed: 0.05,
                },
                gripper_chatter: GripperChatterSettings::default(),
                idle: IdleSettings::default(),
                statistical_anomalies: StatisticalAnomaliesSettings {
                    enabled: true,
                    threshold: 3.5,
                    min_episodes: 5,
                    max_episodes: 200,
                },
                autoencoder_anomalies: AutoencoderAnomaliesSettings {
                    enabled: true,
                    model: AutoencoderChoice::Linear,
                    neurons: 16,
                    train_seconds: 60.0,
                    threshold: 3.5,
                    min_episodes: 20,
                    max_episodes: 200,
                    variance: 0.9,
                    retrain_after: 10,
                },
            }
        );
    }

    #[test]
    fn every_statistic_is_kept_unless_excluded_and_an_inclusion_wins() {
        let selection = parse(
            "qa_params_version = \"1\"\n[statistics]\n\
             exclude = [\"streams.*.lost_upstream_ratio\", \"joints.wrist_*.jerk\"]\n\
             include = [\"streams.camera.lost_upstream_ratio\"]\n",
        )
        .unwrap()
        .statistics;
        for (name, kept) in [
            ("streams.arm.lost_upstream_ratio", false),
            ("streams.camera.lost_upstream_ratio", true),
            ("streams.arm.rate_hz", true),
            ("joints.wrist_1.jerk", false),
            ("joints.wrist_roll.jerk", false),
            ("joints.elbow.jerk", true),
            // `*` stands for part of one name, never for a dot.
            ("joints.wrist_1.tool.jerk", true),
            ("episode.duration_s", true),
        ] {
            assert_eq!(selection.keeps(name), kept, "{name}");
        }
        assert!(StatisticsSelection::default().keeps("joints.elbow.jerk"));
        assert_eq!(
            selection
                .unmatched(&["streams.arm.lost_upstream_ratio", "episode.duration_s"])
                .collect::<Vec<_>>(),
            ["joints.wrist_*.jerk", "streams.camera.lost_upstream_ratio"]
        );
        assert_eq!(
            problems("qa_params_version = \"1\"\n[statistics]\nexclude = [\"joints..jerk\"]\n"),
            [
                "config.invalid_value 3: Invalid configuration value: `exclude` must be a list of statistic names, `*` for any part of one, e.g. \"joints.*.jerk\""
            ]
        );
    }

    #[test]
    fn each_motion_check_is_set_in_the_table_named_after_it() {
        let settings = parse(
            "qa_params_version = \"1\"\n[motion.jump]\nmax_speed = 8\n\
             [motion.frozen]\nenabled = false\nmax_seconds = 0.5\n\
             [motion.chatter]\nmax_closings = 5\n[motion.idle]\nmax_ratio = 0.8\n",
        )
        .unwrap();
        assert_eq!(settings.gripper_chatter.max_closings, 5);
        assert_eq!(settings.idle.max_ratio, 0.8);
        assert_eq!(
            settings.joint_jumps,
            JointJumpsSettings {
                enabled: true,
                max_speed: 8.0,
            }
        );
        assert_eq!(
            settings.frozen_joints,
            FrozenJointsSettings {
                enabled: false,
                max_seconds: 0.5,
                moving_speed: 0.05,
            }
        );
        assert_eq!(
            problems("qa_params_version = \"1\"\n[motion.jump]\nmax_speed = 0\n"),
            [
                "config.invalid_value 3: Invalid configuration value: `max_speed` must be a number from 0.001 to 1000"
            ]
        );
    }

    #[test]
    fn the_autoencoder_is_set_in_its_own_table() {
        let settings = parse(
            "qa_params_version = \"1\"\n\n[anomalies.autoencoder]\nenabled = false\n\
             variance = 0.95\nmin_episodes = 30\nretrain_after = 5\n\
             model = \"neural\"\nneurons = 32\ntrain_seconds = 5\n",
        )
        .unwrap();
        assert_eq!(
            settings.autoencoder_anomalies,
            AutoencoderAnomaliesSettings {
                enabled: false,
                variance: 0.95,
                min_episodes: 30,
                retrain_after: 5,
                model: AutoencoderChoice::Neural,
                neurons: 32,
                train_seconds: 5.0,
                ..AutoencoderAnomaliesSettings::default()
            }
        );
        assert_eq!(
            problems(
                "qa_params_version = \"1\"\n[anomalies.autoencoder]\nvariance = 0.2\n\
                 min_episodes = 300\nmax_episodes = 100\nmodel = \"deep\"\n"
            ),
            [
                "config.invalid_value 6: Invalid configuration value: `model` must be \"linear\" or \"neural\"",
                "config.invalid_value 3: Invalid configuration value: `variance` must be a number from 0.5 to 1",
                "config.invalid_value 4: Invalid configuration value: `min_episodes` (300) is more than `max_episodes` (100): no model would ever be learned",
            ]
        );
    }

    #[test]
    fn anomalies_are_set_in_the_table_named_after_the_detector() {
        let settings = parse(
            "qa_params_version = \"1\"\n\n[anomalies.statistics]\nenabled = false\n\
             threshold = 5\nmin_episodes = 3\nmax_episodes = 50\n",
        )
        .unwrap();
        assert_eq!(
            settings.statistical_anomalies,
            StatisticalAnomaliesSettings {
                enabled: false,
                threshold: 5.0,
                min_episodes: 3,
                max_episodes: 50,
            }
        );
        assert_eq!(
            problems(
                "qa_params_version = \"1\"\n[anomalies.statistics]\nthreshold = 0.5\n\
                 min_episodes = 0\nmax_episode = 3\n"
            ),
            [
                "qa.unknown_key 5: Unknown key in qa.toml: `anomalies.statistics.max_episode` (did you mean `anomalies.statistics.max_episodes`?)",
                "config.invalid_value 3: Invalid configuration value: `threshold` must be a number from 1 to 100",
                "config.invalid_value 4: Invalid configuration value: `min_episodes` must be a whole number from 1 to 1000",
            ]
        );
        assert_eq!(
            problems("qa_params_version = \"1\"\n[anomalies.statistics]\nmax_episodes = 3\n"),
            [
                "config.invalid_value 3: Invalid configuration value: `min_episodes` (5) is more than `max_episodes` (3): no episode would ever be compared"
            ]
        );
    }

    #[test]
    fn each_check_is_set_in_the_table_named_after_it_written_either_way() {
        let tables = parse(
            "qa_params_version = \"arm-tuned-a\"\n\n[streams.complete]\nenabled = false\nmax_dropped = 25\n",
        )
        .unwrap();
        assert_eq!(tables.params_version.as_deref(), Some("arm-tuned-a"));
        assert_eq!(
            tables.streams_complete,
            StreamsCompleteSettings {
                enabled: false,
                max_dropped: 25,
            }
        );
        let dotted =
            parse("qa_params_version = \"arm-tuned-a\"\nstreams.complete.enabled = false\nstreams.complete.max_dropped = 25\n")
                .unwrap();
        assert_eq!(dotted, tables);
    }

    #[test]
    fn every_problem_is_reported_with_its_line() {
        assert_eq!(
            problems(
                "max_dropped = 3\n\n[streams.complete]\nenabled = \"yes\"\nmax_dropped = -1\n\
                 max_dropd = 2\n\n[streams.completed]\nmax_dropped = 1\n"
            ),
            [
                "qa.unknown_key 1: Unknown key in qa.toml: `max_dropped` (did you mean `streams.complete.max_dropped`?)",
                "qa.unknown_key 6: Unknown key in qa.toml: `streams.complete.max_dropd` (did you mean `streams.complete.max_dropped`?)",
                "qa.unknown_key 8: Unknown key in qa.toml: `streams.completed` (did you mean `streams.complete`?)",
                "config.invalid_value 1: Invalid configuration value: `qa.toml` needs `qa_params_version`",
                "config.invalid_value 4: Invalid configuration value: `enabled` must be true or false",
                "config.invalid_value 5: Invalid configuration value: `max_dropped` must be a whole number from 0 to 9223372036854775807",
            ]
        );
        assert_eq!(
            problems("qa_params_version = \"version 3\"\n"),
            [
                "config.invalid_value 1: Invalid configuration value: `qa_params_version` must be text without spaces, e.g. \"3\""
            ]
        );
        assert_eq!(
            problems("qa_params_version = \n")[0].split(' ').next(),
            Some("config.invalid_syntax")
        );
    }

    #[test]
    fn every_qa_toml_example_in_the_quality_page_is_read_without_problems() {
        let page = include_str!("../../../docs/concepts/quality.md");
        let examples: Vec<&str> = page
            .split("```toml\n")
            .skip(1)
            .map(|block| block.split("```").next().unwrap_or_default())
            .collect();
        assert_eq!(examples.len(), 2);
        for example in examples {
            assert!(parse(example).is_ok(), "{example}");
        }
    }
}
