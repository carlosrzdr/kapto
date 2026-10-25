// SPDX-License-Identifier: Apache-2.0

//! The deployment settings Kapto understands.
//!
//! Deployment settings describe how Kapto runs on this machine: where it
//! listens, where it stores data, how much it logs. They can be set in
//! `kapto.toml`, overridden by environment variables, and overridden again by
//! command-line flags.

use crate::value::ValueType;

/// One deployment setting.
#[derive(Debug, PartialEq, Eq)]
pub struct Key {
    /// The setting's name as written in `kapto.toml`: `section.name`.
    pub path: &'static str,
    /// The type of its value.
    pub value_type: ValueType,
    /// What the setting does, for documentation and `config show`.
    pub help: &'static str,
    /// The value used when nothing else sets it.
    pub default: DefaultValue,
}

/// A setting's built-in default.
#[derive(Debug, PartialEq, Eq)]
pub enum DefaultValue {
    /// A fixed value, written as it would be in an environment variable.
    Fixed(&'static str),
    /// A value worked out when Kapto starts; the text says how.
    Computed(&'static str),
}

impl Key {
    /// The `[section]` the setting belongs to.
    #[must_use]
    pub fn section(&self) -> &'static str {
        self.path
            .split_once('.')
            .map_or(self.path, |(section, _)| section)
    }

    /// The setting's name within its section.
    #[must_use]
    pub fn name(&self) -> &'static str {
        self.path
            .split_once('.')
            .map_or(self.path, |(_, name)| name)
    }

    /// The environment variable that overrides it: `KAPTO_` followed by the
    /// path in upper case with dots replaced by underscores.
    #[must_use]
    pub fn env_var(&self) -> String {
        format!("KAPTO_{}", self.path.replace('.', "_").to_ascii_uppercase())
    }
}

/// Every deployment setting, in the order `config show` lists them.
pub const DEPLOYMENT_KEYS: &[Key] = &[
    Key {
        path: "server.bind",
        value_type: ValueType::IpAddress,
        help: "Address the API, web UI and viewer listen on. Loopback by default, so only this machine can reach them.",
        default: DefaultValue::Fixed("127.0.0.1"),
    },
    Key {
        path: "server.port",
        value_type: ValueType::Port,
        help: "Port of the API and web UI.",
        default: DefaultValue::Fixed("7700"),
    },
    Key {
        path: "viewer.stream_port",
        value_type: ValueType::Port,
        help: "Port the viewer bridge streams the robot's poses and plotted values on, as they arrive; the web UI's live view connects to it.",
        default: DefaultValue::Fixed("9877"),
    },
    Key {
        path: "viewer.display_rate",
        value_type: ValueType::Hertz,
        help: "How many times a second the live view moves the robot: a display's refresh rate. Lower it to spend less CPU in the viewer bridge and the browser.",
        default: DefaultValue::Fixed("60"),
    },
    Key {
        path: "viewer.camera_max_fps",
        value_type: ValueType::Hertz,
        help: "How many frames a second, at most, of each camera the recording hands to the live view: the newest each time. Recording keeps every frame whatever this is; lower it to spend less CPU and memory bandwidth on the live view.",
        default: DefaultValue::Fixed("30"),
    },
    Key {
        path: "viewer.page_pictures",
        value_type: ValueType::Hertz,
        help: "How many cameras' pictures a second, at most, the viewer bridge sends each page, shared evenly among its cameras and never more of one than `viewer.camera_max_fps`: with more cameras each is shown less often, so their pictures don't hold up the robot on the page's connection.",
        default: DefaultValue::Fixed("60"),
    },
    Key {
        path: "viewer.history",
        value_type: ValueType::Mebibytes,
        help: "How much recent live data, in MiB, the viewer bridge keeps for the live view to look back over; the oldest goes first.",
        default: DefaultValue::Fixed("64"),
    },
    Key {
        path: "viewer.history_age",
        value_type: ValueType::Seconds,
        help: "How far back, in seconds, the live view can look: the viewer bridge keeps what it streamed for this long, downsampled to the display's rate, unless `viewer.history` fills first.",
        default: DefaultValue::Fixed("600"),
    },
    Key {
        path: "viewer.camera_history",
        value_type: ValueType::Mebibytes,
        help: "How much, in MiB, the viewer bridge keeps of every camera's pictures together, for the live view to look back over: made smaller (`viewer.camera_history_width`) and fewer (`viewer.camera_history_fps`), for `viewer.history_age` unless this fills first; the oldest goes first.",
        default: DefaultValue::Fixed("128"),
    },
    Key {
        path: "viewer.camera_history_fps",
        value_type: ValueType::Hertz,
        help: "How many pictures a second of each camera the viewer bridge keeps for the live view to look back over.",
        default: DefaultValue::Fixed("5"),
    },
    Key {
        path: "viewer.camera_history_width",
        value_type: ValueType::Pixels,
        help: "How wide, at most, in pixels, the pictures the viewer bridge keeps for the live view to look back over are: each is decoded at a half, a quarter or an eighth of its size, whichever first fits.",
        default: DefaultValue::Fixed("640"),
    },
    Key {
        path: "viewer.stop_grace",
        value_type: ValueType::Seconds,
        help: "How long, in seconds, the viewer bridge may take to stop when `kapto serve` stops, before it is killed.",
        default: DefaultValue::Fixed("5"),
    },
    Key {
        path: "viewer.replay_cache_size",
        value_type: ValueType::Mebibytes,
        help: "How much room, in MiB, replays of recorded episodes may take in the data directory's `.cache/replay`. Past it, the replays opened least recently are removed first, but never the one opened last; a replay removed is made again when next opened.",
        default: DefaultValue::Fixed("2048"),
    },
    Key {
        path: "viewer.replay_cache_age",
        value_type: ValueType::Days,
        help: "After how many days without being opened a replay of a recorded episode is removed from the cache, however much room is left. Replays are also all removed when the disk's free space falls below `capture.disk_warn_below`, so they never take the room a recording needs.",
        default: DefaultValue::Fixed("7"),
    },
    Key {
        path: "viewer.tap_socket",
        value_type: ValueType::Path,
        help: "Unix socket the recording sends live samples to, for the viewer. Sending never waits: samples the viewer can't take are dropped and counted. Defaults to `tap.sock` inside the data directory.",
        default: DefaultValue::Computed("`tap.sock` inside the data directory"),
    },
    Key {
        path: "viewer.frame_socket",
        value_type: ValueType::Path,
        help: "Unix socket the recording sends cameras' frames to, for the viewer: frames are too large for `viewer.tap_socket`. Sending never waits: frames the viewer can't take yet are replaced by newer ones and counted. Defaults to `frames.sock` inside the data directory.",
        default: DefaultValue::Computed("`frames.sock` inside the data directory"),
    },
    Key {
        path: "capture.control_socket",
        value_type: ValueType::Path,
        help: "Unix socket the capture process takes its orders on: start and stop episodes, report status. Defaults to `capture.sock` inside the data directory.",
        default: DefaultValue::Computed("`capture.sock` inside the data directory"),
    },
    Key {
        path: "capture.control_timeout",
        value_type: ValueType::Seconds,
        help: "How long, in seconds, the capture process keeps recording after `kapto serve` has gone, for example because it crashed, before it finalises the episode. Long enough for serve to be started again and carry on with the episode. A capture process serve started then ends, as it does whenever serve has gone and no episode is recorded.",
        default: DefaultValue::Fixed("60"),
    },
    Key {
        path: "capture.stop_grace",
        value_type: ValueType::Seconds,
        help: "How long, in seconds, the capture process may take to stop when `kapto serve` stops, before it is killed: long enough to finalise the episode being recorded.",
        default: DefaultValue::Fixed("30"),
    },
    Key {
        path: "capture.disk_warn_below",
        value_type: ValueType::Mebibytes,
        help: "Below how much free space, in MiB, on the disk holding the data directory Kapto warns that it is running low, live and in the web UI.",
        default: DefaultValue::Fixed("10240"),
    },
    Key {
        path: "capture.disk_stop_below",
        value_type: ValueType::Mebibytes,
        help: "Below how much free space, in MiB, on the disk holding the data directory the capture process finalises the episode being recorded and starts no other, so a recording is never cut off by a full disk. Leave room for what other programs write.",
        default: DefaultValue::Fixed("2048"),
    },
    Key {
        path: "capture.pre_roll_max",
        value_type: ValueType::Mebibytes,
        help: "The most memory, in MiB, set aside for one stream's pre-roll (`pre_roll_s` in `streams.toml`): what it needs for its seconds at its rate and largest message, up to this. A stream whose messages are larger than that allows holds fewer seconds; the capture process says the total it sets aside when it starts.",
        default: DefaultValue::Fixed("64"),
    },
    Key {
        path: "capture.preflight_window",
        value_type: ValueType::Seconds,
        help: "How long, in seconds, pre-flight watches the streams to check each arrives at roughly its declared rate. Pre-flight runs when the capture process starts and whenever asked.",
        default: DefaultValue::Fixed("5"),
    },
    Key {
        path: "capture.preflight_disk_time",
        value_type: ValueType::Seconds,
        help: "How long a recording, in seconds, pre-flight checks the disk has room for at the streams' rate, above `capture.disk_stop_below`.",
        default: DefaultValue::Fixed("600"),
    },
    Key {
        path: "capture.preflight",
        value_type: ValueType::PreflightFailures,
        help: "What a failed pre-flight check does to starting an episode: `refuse` refuses the start unless it waives each failed check, `warn` starts anyway. Either way the episode says which checks had failed and which were waived.",
        default: DefaultValue::Fixed("refuse"),
    },
    Key {
        path: "capture.compression",
        value_type: ValueType::Compression,
        help: "How each recording's chunks are compressed: `lz4`, fast; `zstd`, smaller for more CPU; `none`, stored as received. A chunk holds every stream's messages, so the choice is the whole recording's: `none` saves the CPU spent on messages already compressed, such as a camera's JPEG frames, at the cost of the other streams stored as they are.",
        default: DefaultValue::Fixed("lz4"),
    },
    Key {
        path: "capture.attach_meshes",
        value_type: ValueType::Switch,
        help: "Whether each recording also carries the robot package's meshes, and the textures and buffers they load. Every other file of the package is always attached; meshes are left out by default because a real robot's can take tens of megabytes per episode, and each one left out is named in the recording with its SHA-256. With `true`, the recording alone can draw the robot.",
        default: DefaultValue::Fixed("false"),
    },
    Key {
        path: "bin.keep_days",
        value_type: ValueType::Days,
        help: "How many days an episode deleted by hand stays in the bin, where it can be put back, before it is deleted for good. The oldest goes sooner while the disk's free space is below `capture.disk_warn_below`, and every one when the bin is emptied by hand.",
        default: DefaultValue::Fixed("7"),
    },
    Key {
        path: "supervisor.restart_delay",
        value_type: ValueType::Seconds,
        help: "How long, in seconds, `kapto serve` waits before starting a process that exited again. The wait doubles each time the process fails again soon after starting, so one that can't start doesn't spin.",
        default: DefaultValue::Fixed("1"),
    },
    Key {
        path: "supervisor.restart_max_delay",
        value_type: ValueType::Seconds,
        help: "The longest wait, in seconds, before `kapto serve` starts a process again, however often it failed.",
        default: DefaultValue::Fixed("30"),
    },
    Key {
        path: "supervisor.stable_after",
        value_type: ValueType::Seconds,
        help: "How long, in seconds, a process must have run for its exit to count as a new failure rather than one of a series: it is then started again after `supervisor.restart_delay`, not the doubled wait.",
        default: DefaultValue::Fixed("60"),
    },
    Key {
        path: "worker.max_attempts",
        value_type: ValueType::Count,
        help: "How many times the worker may start a job. A job left unfinished because the worker or `kapto serve` stopped is started again when serve starts, until it has been started this many times; it then fails with `worker.job_interrupted`, so a job that brings the worker down can't do so forever.",
        default: DefaultValue::Fixed("3"),
    },
    Key {
        path: "worker.pause_while_recording",
        value_type: ValueType::Switch,
        help: "Whether the worker waits while an episode is recorded: it starts no job, and a job it is doing waits at its next step until the episode ends, so recording has the CPU and the disk to itself. With `false`, jobs go on during recording, at the worker's low priority.",
        default: DefaultValue::Fixed("true"),
    },
    Key {
        path: "upload.when",
        value_type: ValueType::UploadWhen,
        help: "Which episodes are uploaded by themselves, to every destination: `never` (only by hand), `finalised` (every finished episode, with warnings or not), `finalised_clean` (finished with nothing lost, dropped or recovered, and pre-flight passed) or `accepted` (no quality gate failed). Any episode can still be uploaded by hand.",
        default: DefaultValue::Fixed("finalised"),
    },
    Key {
        path: "upload.pause_while_recording",
        value_type: ValueType::Switch,
        help: "Whether uploads wait while an episode is recorded, so recording has the disk and the network to itself. With `false`, uploads go on during recording, at the worker's low priority.",
        default: DefaultValue::Fixed("true"),
    },
    Key {
        path: "upload.delete_local",
        value_type: ValueType::DeleteLocal,
        help: "What happens to an episode's local copy once it is uploaded: `when_disk_low` (the oldest uploaded episodes are deleted when the disk runs low), `never`, or `after_upload` (as soon as it is verified). An episode not verified on `upload.min_verified_destinations` destinations is never deleted.",
        default: DefaultValue::Fixed("when_disk_low"),
    },
    Key {
        path: "upload.min_verified_destinations",
        value_type: ValueType::Count,
        help: "On how many destinations an episode must be verified, its completeness marker written, before its local copy may be deleted.",
        default: DefaultValue::Fixed("1"),
    },
    Key {
        path: "upload.verify",
        value_type: ValueType::Verification,
        help: "How each uploaded file is checked to be the one sent: `server_checksum` (the store checks its SHA-256 itself; a store that can't has the file read back instead), `read_back` (read back and hashed again, twice the traffic) or `size_only`.",
        default: DefaultValue::Fixed("server_checksum"),
    },
    Key {
        path: "upload.marker",
        value_type: ValueType::Switch,
        help: "Whether the completeness marker `_SUCCESS` is written last in each uploaded episode, listing every file with its size and SHA-256. It is how anyone reading the bucket knows an episode is complete; turned off, they have no such rule, though Kapto still verifies what it uploads.",
        default: DefaultValue::Fixed("true"),
    },
    Key {
        path: "upload.part_size",
        value_type: ValueType::Mebibytes,
        help: "The size, in MiB, of the parts a large file is uploaded in. Lowered to about one second at `upload.bandwidth` when that is set, so a part never leaves as a burst much longer than a second; raised for a file that would need more parts than the store allows, and never below the smallest part it accepts (5 MiB on S3 and Google Cloud Storage).",
        default: DefaultValue::Fixed("16"),
    },
    Key {
        path: "upload.parallel_parts",
        value_type: ValueType::Count,
        help: "How many parts of one file are uploaded at once to begin with. More go at once while the store keeps up, up to `upload.max_in_flight`, and half as many when it asks for fewer requests.",
        default: DefaultValue::Fixed("4"),
    },
    Key {
        path: "upload.parallel_files",
        value_type: ValueType::Count,
        help: "How many files of an episode are uploaded at once. Not used yet: files go up one at a time, a large one already in as many parts at once as the store keeps up with.",
        default: DefaultValue::Fixed("2"),
    },
    Key {
        path: "upload.max_in_flight",
        value_type: ValueType::Count,
        help: "The most requests to a destination in flight at once. Uploads start at `upload.parallel_parts` and send one more at once for each round of requests that succeed, up to this; when the store asks for fewer (a `429`, a `503`, S3's `SlowDown`), they halve what is in flight and try the request again after a wait, without counting it as a failed attempt.",
        default: DefaultValue::Fixed("32"),
    },
    Key {
        path: "upload.bandwidth",
        value_type: ValueType::Bandwidth,
        help: "The most an upload may send, in MiB/s on average, for a link shared with the robot's own traffic; 0 for no cap. Each request waits until those before it would have taken at the cap, then goes as fast as the link takes it.",
        default: DefaultValue::Fixed("0"),
    },
    Key {
        path: "upload.max_attempts",
        value_type: ValueType::Count,
        help: "How many times a file is tried before its episode's upload fails. A request the store asked to slow down for doesn't count.",
        default: DefaultValue::Fixed("8"),
    },
    Key {
        path: "upload.retry_failed_after",
        value_type: ValueType::Seconds,
        help: "How long, in seconds, before an episode whose upload failed is tried again by itself.",
        default: DefaultValue::Fixed("3600"),
    },
    Key {
        path: "upload.backoff_initial",
        value_type: ValueType::Seconds,
        help: "The first wait, in seconds, before a failed request is tried again. Each wait after doubles, with a random part, up to `upload.backoff_max`.",
        default: DefaultValue::Fixed("0.5"),
    },
    Key {
        path: "upload.backoff_max",
        value_type: ValueType::Seconds,
        help: "The longest wait, in seconds, before a failed request is tried again.",
        default: DefaultValue::Fixed("300"),
    },
    Key {
        path: "upload.timeout_connect",
        value_type: ValueType::Seconds,
        help: "How long, in seconds, connecting to a destination may take.",
        default: DefaultValue::Fixed("10"),
    },
    Key {
        path: "upload.timeout_request",
        value_type: ValueType::Seconds,
        help: "How long, in seconds, one request to a destination may take, a part's included.",
        default: DefaultValue::Fixed("60"),
    },
    Key {
        path: "paths.data",
        value_type: ValueType::Path,
        help: "Where recordings and Kapto's runtime state are stored.",
        default: DefaultValue::Computed(
            "`$XDG_DATA_HOME/kapto`, else `~/.local/share/kapto`, else `kapto-data` in the working directory",
        ),
    },
    Key {
        path: "paths.packages",
        value_type: ValueType::Path,
        help: "Where robot packages are stored. Defaults to `packages` inside the data directory.",
        default: DefaultValue::Computed("`packages` inside the data directory"),
    },
];

/// The setting with the given `section.name` path.
#[must_use]
pub fn find(path: &str) -> Option<&'static Key> {
    DEPLOYMENT_KEYS.iter().find(|key| key.path == path)
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::*;

    #[test]
    fn every_setting_is_a_section_and_a_name() {
        for key in DEPLOYMENT_KEYS {
            assert_eq!(key.path, format!("{}.{}", key.section(), key.name()));
            assert!(!key.name().contains('.'), "{} is nested too deep", key.path);
        }
    }

    #[test]
    fn setting_paths_and_environment_variables_are_unique() {
        let paths: HashSet<_> = DEPLOYMENT_KEYS.iter().map(|key| key.path).collect();
        let vars: HashSet<_> = DEPLOYMENT_KEYS.iter().map(Key::env_var).collect();
        assert_eq!(paths.len(), DEPLOYMENT_KEYS.len());
        assert_eq!(vars.len(), DEPLOYMENT_KEYS.len());
    }

    #[test]
    fn environment_variables_are_derived_from_the_path() {
        assert_eq!(find("server.port").unwrap().env_var(), "KAPTO_SERVER_PORT");
        assert_eq!(find("paths.data").unwrap().env_var(), "KAPTO_PATHS_DATA");
    }

    #[test]
    fn every_fixed_default_is_a_valid_value() {
        for key in DEPLOYMENT_KEYS {
            if let DefaultValue::Fixed(text) = key.default {
                assert!(
                    key.value_type.parse_text(text).is_some(),
                    "{} has an invalid default {text:?}",
                    key.path
                );
            }
        }
    }

    #[test]
    fn every_setting_explains_itself() {
        for key in DEPLOYMENT_KEYS {
            assert!(
                key.help.ends_with('.'),
                "{} needs a help sentence",
                key.path
            );
        }
    }
}
