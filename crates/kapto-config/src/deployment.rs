// SPDX-License-Identifier: Apache-2.0

//! Resolving deployment settings from defaults, the configuration file,
//! environment variables and command-line flags.

use std::collections::{BTreeMap, BTreeSet};
use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::time::Duration;

use kapto_core::{ErrorCode, KaptoError};
use toml_edit::Document;

use crate::destination::{self, UploadDestination};
use crate::file::ConfigFile;
use crate::keys::{DEPLOYMENT_KEYS, DefaultValue, Key, find};
use crate::located::{at_line, line_of, syntax_error, unknown_key};
use crate::source::{Effective, Source};
use crate::suggest::closest;
use crate::upload::UploadSettings;
use crate::value::{Compression, PreflightFailures, Value, ValueType};

/// A setting given on the command line, e.g. `--port 7701` for `server.port`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CliOverride {
    /// The setting's path, e.g. `server.port`.
    pub key: &'static str,
    /// The value as typed.
    pub value: String,
    /// The flag as typed, e.g. `--port`, shown as the value's source.
    pub flag: String,
}

/// Everything deployment settings are resolved from.
pub struct Inputs<'a> {
    /// The configuration file, if there is one.
    pub file: Option<&'a ConfigFile>,
    /// Looks up an environment variable.
    pub env: &'a dyn Fn(&str) -> Option<String>,
    /// Settings given as command-line flags.
    pub cli: &'a [CliOverride],
}

/// How Kapto runs on this machine, with the source of every value.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeploymentConfig {
    /// Address the API and web UI listen on.
    pub server_bind: Effective<IpAddr>,
    /// Port of the API and web UI.
    pub server_port: Effective<u16>,
    /// Port the viewer bridge streams poses and plotted values on.
    pub viewer_stream_port: Effective<u16>,
    /// How far back the live view can look.
    pub viewer_history_age: Effective<Duration>,
    /// How many times a second the live view moves the robot.
    pub viewer_display_rate: Effective<u32>,
    /// How many frames a second, at most, of each camera reach the live
    /// view.
    pub viewer_camera_max_fps: Effective<u32>,
    /// How much recent live data the viewer bridge keeps, in MiB.
    pub viewer_history_mib: Effective<u64>,
    /// How long the viewer bridge may take to stop.
    pub viewer_stop_grace: Effective<Duration>,
    /// How much room replays may take in the cache, in MiB.
    pub replay_cache_size_mib: Effective<u64>,
    /// After how many days unopened a replay is removed.
    pub replay_cache_age_days: Effective<u32>,
    /// Unix socket the recording sends live samples to, for the viewer.
    pub tap_socket: Effective<PathBuf>,
    /// Unix socket the recording sends cameras' frames to, for the viewer.
    pub frame_socket: Effective<PathBuf>,
    /// Unix socket the capture process takes its orders on.
    pub control_socket: Effective<PathBuf>,
    /// How long capture keeps recording after `kapto serve` has gone.
    pub control_timeout: Effective<Duration>,
    /// How long the capture process may take to stop.
    pub capture_stop_grace: Effective<Duration>,
    /// Below how much free space, in MiB, Kapto warns.
    pub disk_warn_below_mib: Effective<u64>,
    /// Below how much free space, in MiB, recording stops.
    pub disk_stop_below_mib: Effective<u64>,
    /// The most memory, in MiB, set aside for one stream's pre-roll.
    pub pre_roll_max: Effective<u64>,
    /// How long pre-flight watches the streams.
    pub preflight_window: Effective<Duration>,
    /// How long a recording pre-flight checks the disk has room for.
    pub preflight_disk_time: Effective<Duration>,
    /// What a failed pre-flight check does to starting an episode.
    pub preflight_failures: Effective<PreflightFailures>,
    /// Whether recordings also carry the package's meshes.
    pub attach_meshes: Effective<bool>,
    /// How each recording's chunks are compressed.
    pub compression: Effective<Compression>,
    /// The wait before a process that exited is started again.
    pub restart_delay: Effective<Duration>,
    /// The longest wait before a process is started again.
    pub restart_max_delay: Effective<Duration>,
    /// How long a process must have run for its exit to start a new series
    /// of waits.
    pub stable_after: Effective<Duration>,
    /// How many times the worker may start a job.
    pub worker_max_attempts: Effective<u32>,
    /// Whether the worker waits while an episode is recorded.
    pub worker_pause_while_recording: Effective<bool>,
    /// How episodes are uploaded.
    pub upload: UploadSettings,
    /// Where episodes are uploaded, each with its own settings.
    pub upload_destinations: Vec<UploadDestination>,
    /// Where recordings and runtime state are stored.
    pub data_dir: Effective<PathBuf>,
    /// Where robot packages are stored.
    pub packages_dir: Effective<PathBuf>,
}

/// One resolved setting, for listing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    /// The setting.
    pub key: &'static Key,
    /// Its value as text.
    pub value: String,
    /// Where the value came from.
    pub source: Source,
}

type Values = BTreeMap<&'static str, Effective<Value>>;

/// Settings that were given an invalid value in some layer.
type Invalid = BTreeSet<&'static str>;

impl DeploymentConfig {
    /// Resolves every setting. Each layer overrides the one before: built-in
    /// default, configuration file, environment variable, command-line flag.
    ///
    /// # Errors
    ///
    /// Every problem found, so they can all be fixed at once: invalid TOML,
    /// unknown settings, invalid values, unparseable environment variables,
    /// and two listeners on the same port.
    pub fn resolve(inputs: &Inputs<'_>) -> Result<Self, Vec<KaptoError>> {
        let mut values = Values::new();
        let mut errors = Vec::new();
        let mut invalid = Invalid::new();

        if let Some(file) = inputs.file {
            read_file(file, &mut values, &mut errors, &mut invalid);
        }
        read_env(inputs.env, &mut values, &mut errors, &mut invalid);
        read_cli(inputs.cli, &mut values, &mut errors, &mut invalid);
        add_defaults(inputs.env, &mut values);
        // Checked even when other settings are wrong, so every problem is
        // reported in one pass.
        errors.extend(port_conflicts(&values, &invalid));
        let upload_destinations = inputs
            .file
            .map(|file| destinations(file, &values, &mut errors))
            .unwrap_or_default();
        if !errors.is_empty() {
            return Err(errors);
        }

        let config = Self {
            server_bind: take(&mut values, "server.bind", |value| match value {
                Value::IpAddress(address) => Some(address),
                _ => None,
            }),
            server_port: take(&mut values, "server.port", |value| match value {
                Value::Port(port) => Some(port),
                _ => None,
            }),
            viewer_stream_port: take(&mut values, "viewer.stream_port", |value| match value {
                Value::Port(port) => Some(port),
                _ => None,
            }),
            viewer_display_rate: take(&mut values, "viewer.display_rate", hertz),
            viewer_camera_max_fps: take(&mut values, "viewer.camera_max_fps", hertz),
            viewer_history_mib: take(&mut values, "viewer.history", mebibytes),
            viewer_history_age: take(&mut values, "viewer.history_age", seconds),
            viewer_stop_grace: take(&mut values, "viewer.stop_grace", seconds),
            replay_cache_size_mib: take(&mut values, "viewer.replay_cache_size", mebibytes),
            replay_cache_age_days: take(&mut values, "viewer.replay_cache_age", days),
            tap_socket: take(&mut values, "viewer.tap_socket", path),
            frame_socket: take(&mut values, "viewer.frame_socket", path),
            control_socket: take(&mut values, "capture.control_socket", path),
            control_timeout: take(&mut values, "capture.control_timeout", seconds),
            capture_stop_grace: take(&mut values, "capture.stop_grace", seconds),
            disk_warn_below_mib: take(&mut values, "capture.disk_warn_below", mebibytes),
            disk_stop_below_mib: take(&mut values, "capture.disk_stop_below", mebibytes),
            pre_roll_max: take(&mut values, "capture.pre_roll_max", mebibytes),
            preflight_window: take(&mut values, "capture.preflight_window", seconds),
            preflight_disk_time: take(&mut values, "capture.preflight_disk_time", seconds),
            preflight_failures: take(&mut values, "capture.preflight", |value| match value {
                Value::PreflightFailures(failures) => Some(failures),
                _ => None,
            }),
            compression: take(&mut values, "capture.compression", |value| match value {
                Value::Compression(compression) => Some(compression),
                _ => None,
            }),
            attach_meshes: take(&mut values, "capture.attach_meshes", switch),
            restart_delay: take(&mut values, "supervisor.restart_delay", seconds),
            restart_max_delay: take(&mut values, "supervisor.restart_max_delay", seconds),
            stable_after: take(&mut values, "supervisor.stable_after", seconds),
            worker_max_attempts: take(&mut values, "worker.max_attempts", |value| match value {
                Value::Count(count) => Some(count),
                _ => None,
            }),
            worker_pause_while_recording: take(&mut values, "worker.pause_while_recording", switch),
            upload: upload_settings(&mut values),
            upload_destinations,
            data_dir: take(&mut values, "paths.data", path),
            packages_dir: take(&mut values, "paths.packages", path),
        };
        Ok(config)
    }

    /// Every setting with its value and source, in documentation order.
    #[must_use]
    pub fn entries(&self) -> Vec<Entry> {
        DEPLOYMENT_KEYS
            .iter()
            .map(|key| {
                let (value, source) = match key.path {
                    "server.bind" => text(&self.server_bind),
                    "server.port" => text(&self.server_port),
                    "viewer.stream_port" => text(&self.viewer_stream_port),
                    "viewer.display_rate" => text(&self.viewer_display_rate),
                    "viewer.camera_max_fps" => text(&self.viewer_camera_max_fps),
                    "viewer.history" => text(&self.viewer_history_mib),
                    "viewer.history_age" => seconds_text(&self.viewer_history_age),
                    "viewer.stop_grace" => seconds_text(&self.viewer_stop_grace),
                    "viewer.replay_cache_size" => text(&self.replay_cache_size_mib),
                    "viewer.replay_cache_age" => text(&self.replay_cache_age_days),
                    "viewer.tap_socket" => display_path(&self.tap_socket),
                    "viewer.frame_socket" => display_path(&self.frame_socket),
                    "capture.control_socket" => display_path(&self.control_socket),
                    "capture.control_timeout" => seconds_text(&self.control_timeout),
                    "capture.stop_grace" => seconds_text(&self.capture_stop_grace),
                    "capture.disk_warn_below" => text(&self.disk_warn_below_mib),
                    "capture.disk_stop_below" => text(&self.disk_stop_below_mib),
                    "capture.pre_roll_max" => text(&self.pre_roll_max),
                    "capture.preflight_window" => seconds_text(&self.preflight_window),
                    "capture.preflight_disk_time" => seconds_text(&self.preflight_disk_time),
                    "capture.preflight" => text(&self.preflight_failures),
                    "capture.attach_meshes" => text(&self.attach_meshes),
                    "capture.compression" => text(&self.compression),
                    "supervisor.restart_delay" => seconds_text(&self.restart_delay),
                    "supervisor.restart_max_delay" => seconds_text(&self.restart_max_delay),
                    "supervisor.stable_after" => seconds_text(&self.stable_after),
                    "worker.max_attempts" => text(&self.worker_max_attempts),
                    "worker.pause_while_recording" => text(&self.worker_pause_while_recording),
                    "upload.when" => text(&self.upload.when),
                    "upload.pause_while_recording" => text(&self.upload.pause_while_recording),
                    "upload.delete_local" => text(&self.upload.delete_local),
                    "upload.min_verified_destinations" => {
                        text(&self.upload.min_verified_destinations)
                    }
                    "upload.verify" => text(&self.upload.verify),
                    "upload.marker" => text(&self.upload.marker),
                    "upload.part_size" => text(&self.upload.part_size_mib),
                    "upload.parallel_parts" => text(&self.upload.parallel_parts),
                    "upload.parallel_files" => text(&self.upload.parallel_files),
                    "upload.max_in_flight" => text(&self.upload.max_in_flight),
                    "upload.bandwidth" => text(&self.upload.bandwidth_mib),
                    "upload.max_attempts" => text(&self.upload.max_attempts),
                    "upload.retry_failed_after" => seconds_text(&self.upload.retry_failed_after),
                    "upload.backoff_initial" => seconds_text(&self.upload.backoff_initial),
                    "upload.backoff_max" => seconds_text(&self.upload.backoff_max),
                    "upload.timeout_connect" => seconds_text(&self.upload.timeout_connect),
                    "upload.timeout_request" => seconds_text(&self.upload.timeout_request),
                    "paths.data" => display_path(&self.data_dir),
                    "paths.packages" => display_path(&self.packages_dir),
                    other => unreachable!("{other} is listed but has no field"),
                };
                Entry { key, value, source }
            })
            .collect()
    }
}

/// Kapto never moves a listener to another port by itself, so two listeners
/// configured on one port is an error rather than a conflict to settle at
/// startup. Settings with an invalid value are skipped: their real value is
/// unknown, so a conflict with the fallback would be misleading.
fn port_conflicts(values: &Values, invalid: &Invalid) -> Vec<KaptoError> {
    let ports: Vec<(&str, u16, &Source)> = DEPLOYMENT_KEYS
        .iter()
        .filter(|key| key.value_type == ValueType::Port && !invalid.contains(key.path))
        .filter_map(|key| match values.get(key.path) {
            Some(Effective {
                value: Value::Port(port),
                source,
            }) => Some((key.path, *port, source)),
            _ => None,
        })
        .collect();
    let mut errors = Vec::new();
    for (index, &(first, port, first_source)) in ports.iter().enumerate() {
        for &(second, other, second_source) in &ports[index + 1..] {
            if port == other {
                errors.push(
                    KaptoError::new(ErrorCode::ConfigPortConflict)
                        .detail(format!("{first} and {second} are both {port}"))
                        .context(first, first_source.to_string())
                        .context(second, second_source.to_string()),
                );
            }
        }
    }
    errors
}

/// The `[[upload.destination]]` entries of `file`, each with the `[upload]`
/// settings in `values` and what it repeats of them; their problems go to
/// `errors`.
fn destinations(
    file: &ConfigFile,
    values: &Values,
    errors: &mut Vec<KaptoError>,
) -> Vec<UploadDestination> {
    let Ok(document) = Document::parse(file.contents.as_str()) else {
        return Vec::new();
    };
    let Some(entries) = document
        .get("upload")
        .and_then(|upload| upload.get("destination"))
    else {
        return Vec::new();
    };
    let upload: Values = values
        .iter()
        .filter(|(key, _)| key.starts_with("upload."))
        .map(|(key, value)| (*key, value.clone()))
        .collect();
    let base = upload_settings(&mut upload.clone());
    let names: Vec<&str> = DEPLOYMENT_KEYS
        .iter()
        .filter(|key| key.section() == "upload")
        .map(Key::name)
        .collect();
    let read = destination::read(file, entries, &base, &names, errors);
    read.into_iter()
        .map(|entry| {
            let mut own = upload.clone();
            for (name, item, line) in entry.overrides {
                let Some(key) = find(&format!("upload.{name}")) else {
                    continue;
                };
                let parsed = item
                    .as_value()
                    .and_then(|value| key.value_type.parse_toml(value));
                let Some(value) = parsed else {
                    let error = KaptoError::new(ErrorCode::ConfigInvalidValue).detail(format!(
                        "`{name}` of destination `{}` must be {}",
                        entry.destination.name,
                        key.value_type.expected()
                    ));
                    errors.push(at_line(error, file, line));
                    continue;
                };
                let source = Source::File {
                    path: file.path.clone(),
                    line,
                };
                own.insert(key.path, Effective { value, source });
            }
            UploadDestination {
                settings: upload_settings(&mut own),
                ..entry.destination
            }
        })
        .collect()
}

fn upload_settings(values: &mut Values) -> UploadSettings {
    UploadSettings {
        when: take(values, "upload.when", |value| match value {
            Value::UploadWhen(when) => Some(when),
            _ => None,
        }),
        pause_while_recording: take(values, "upload.pause_while_recording", switch),
        delete_local: take(values, "upload.delete_local", |value| match value {
            Value::DeleteLocal(delete) => Some(delete),
            _ => None,
        }),
        min_verified_destinations: take(values, "upload.min_verified_destinations", count),
        verify: take(values, "upload.verify", |value| match value {
            Value::Verification(verify) => Some(verify),
            _ => None,
        }),
        marker: take(values, "upload.marker", switch),
        part_size_mib: take(values, "upload.part_size", mebibytes),
        parallel_parts: take(values, "upload.parallel_parts", count),
        parallel_files: take(values, "upload.parallel_files", count),
        max_in_flight: take(values, "upload.max_in_flight", count),
        bandwidth_mib: take(values, "upload.bandwidth", |value| match value {
            Value::Bandwidth(rate) => Some(rate),
            _ => None,
        }),
        max_attempts: take(values, "upload.max_attempts", count),
        retry_failed_after: take(values, "upload.retry_failed_after", seconds),
        backoff_initial: take(values, "upload.backoff_initial", seconds),
        backoff_max: take(values, "upload.backoff_max", seconds),
        timeout_connect: take(values, "upload.timeout_connect", seconds),
        timeout_request: take(values, "upload.timeout_request", seconds),
    }
}

#[allow(
    clippy::needless_pass_by_value,
    reason = "a conversion for `take`, which hands over the value"
)]
fn switch(value: Value) -> Option<bool> {
    match value {
        Value::Switch(on) => Some(on),
        _ => None,
    }
}

#[allow(
    clippy::needless_pass_by_value,
    reason = "a conversion for `take`, which hands over the value"
)]
fn count(value: Value) -> Option<u32> {
    match value {
        Value::Count(count) => Some(count),
        _ => None,
    }
}

fn text<T: ToString>(effective: &Effective<T>) -> (String, Source) {
    (effective.value.to_string(), effective.source.clone())
}

fn display_path(effective: &Effective<PathBuf>) -> (String, Source) {
    (
        effective.value.display().to_string(),
        effective.source.clone(),
    )
}

fn seconds_text(effective: &Effective<Duration>) -> (String, Source) {
    (
        Value::Seconds(effective.value).to_string(),
        effective.source.clone(),
    )
}

#[allow(
    clippy::needless_pass_by_value,
    reason = "a conversion for `take`, which hands over the value"
)]
fn seconds(value: Value) -> Option<Duration> {
    match value {
        Value::Seconds(time) => Some(time),
        _ => None,
    }
}

#[allow(
    clippy::needless_pass_by_value,
    reason = "a conversion for `take`, which hands over the value"
)]
fn mebibytes(value: Value) -> Option<u64> {
    match value {
        Value::Mebibytes(size) => Some(size),
        _ => None,
    }
}

#[allow(
    clippy::needless_pass_by_value,
    reason = "a conversion for `take`, which hands over the value"
)]
fn hertz(value: Value) -> Option<u32> {
    match value {
        Value::Hertz(rate) => Some(rate),
        _ => None,
    }
}

#[allow(
    clippy::needless_pass_by_value,
    reason = "a conversion for `take`, which hands over the value"
)]
fn days(value: Value) -> Option<u32> {
    match value {
        Value::Days(days) => Some(days),
        _ => None,
    }
}

fn path(value: Value) -> Option<PathBuf> {
    match value {
        Value::Path(path) => Some(path),
        _ => None,
    }
}

/// Removes a resolved setting and converts it to its field type. Every
/// setting is present after defaults are added, and was parsed with its own
/// type, so neither can fail.
fn take<T>(values: &mut Values, key: &str, convert: fn(Value) -> Option<T>) -> Effective<T> {
    let Some(Effective { value, source }) = values.remove(key) else {
        unreachable!("{key} has a default, so it is always resolved");
    };
    let Some(value) = convert(value) else {
        unreachable!("{key} is always parsed as its declared type");
    };
    Effective { value, source }
}

fn read_file(
    file: &ConfigFile,
    values: &mut Values,
    errors: &mut Vec<KaptoError>,
    invalid: &mut Invalid,
) {
    let document = match Document::parse(file.contents.as_str()) {
        Ok(document) => document,
        Err(error) => {
            errors.push(syntax_error(file, &error));
            return;
        }
    };
    let Some(root) = document.as_item().as_table_like() else {
        return;
    };
    let sections: Vec<&str> = DEPLOYMENT_KEYS.iter().map(Key::section).collect();

    for (section, item) in root.iter() {
        let line = line_of(file, root, section);
        if !sections.contains(&section) {
            let suggestion = closest(section, sections.iter().copied());
            errors.push(at_line(
                unknown_key(ErrorCode::ConfigUnknownKey, section, suggestion),
                file,
                line,
            ));
            continue;
        }
        let Some(table) = item.as_table_like() else {
            let error = KaptoError::new(ErrorCode::ConfigInvalidValue)
                .detail(format!("`{section}` must be a [{section}] table"));
            errors.push(at_line(error, file, line));
            continue;
        };
        for (name, item) in table.iter() {
            // Read on their own once every setting is known.
            if section == "upload" && name == "destination" {
                continue;
            }
            let path = format!("{section}.{name}");
            let line = line_of(file, table, name);
            let Some(key) = find(&path) else {
                let names = DEPLOYMENT_KEYS
                    .iter()
                    .filter(|key| key.section() == section)
                    .map(Key::name);
                let suggestion = closest(name, names).map(|name| format!("{section}.{name}"));
                let error = unknown_key(ErrorCode::ConfigUnknownKey, &path, suggestion.as_deref());
                errors.push(at_line(error, file, line));
                continue;
            };
            let parsed = item
                .as_value()
                .and_then(|value| key.value_type.parse_toml(value));
            let Some(value) = parsed else {
                invalid.insert(key.path);
                let error = KaptoError::new(ErrorCode::ConfigInvalidValue).detail(format!(
                    "`{}` must be {}",
                    key.path,
                    key.value_type.expected()
                ));
                errors.push(at_line(error, file, line));
                continue;
            };
            values.insert(
                key.path,
                Effective {
                    value,
                    source: Source::File {
                        path: file.path.clone(),
                        line,
                    },
                },
            );
        }
    }
}

fn read_env(
    env: &dyn Fn(&str) -> Option<String>,
    values: &mut Values,
    errors: &mut Vec<KaptoError>,
    invalid: &mut Invalid,
) {
    for key in DEPLOYMENT_KEYS {
        let var = key.env_var();
        let Some(text) = env(&var) else {
            continue;
        };
        let parsed = key.value_type.parse_text(&text);
        if parsed.is_none() {
            invalid.insert(key.path);
        }
        match parsed {
            Some(value) => {
                values.insert(
                    key.path,
                    Effective {
                        value,
                        source: Source::Env { var },
                    },
                );
            }
            None => errors.push(
                KaptoError::new(ErrorCode::ConfigInvalidEnvVar)
                    .detail(format!(
                        "{var}={text:?} must be {}",
                        key.value_type.expected()
                    ))
                    .context("setting", key.path),
            ),
        }
    }
}

fn read_cli(
    cli: &[CliOverride],
    values: &mut Values,
    errors: &mut Vec<KaptoError>,
    invalid: &mut Invalid,
) {
    for flag in cli {
        let Some(key) = find(flag.key) else {
            errors.push(
                KaptoError::new(ErrorCode::ConfigUnknownKey)
                    .detail(format!("`{}`", flag.key))
                    .context("flag", flag.flag.clone()),
            );
            continue;
        };
        let parsed = key.value_type.parse_text(&flag.value);
        if parsed.is_none() {
            invalid.insert(key.path);
        }
        match parsed {
            Some(value) => {
                values.insert(
                    key.path,
                    Effective {
                        value,
                        source: Source::Cli {
                            flag: flag.flag.clone(),
                        },
                    },
                );
            }
            None => errors.push(
                KaptoError::new(ErrorCode::ConfigInvalidValue)
                    .detail(format!(
                        "{} {:?} must be {}",
                        flag.flag,
                        flag.value,
                        key.value_type.expected()
                    ))
                    .context("setting", key.path),
            ),
        }
    }
}

fn add_defaults(env: &dyn Fn(&str) -> Option<String>, values: &mut Values) {
    for key in DEPLOYMENT_KEYS {
        if let DefaultValue::Fixed(text) = key.default {
            let Some(value) = key.value_type.parse_text(text) else {
                unreachable!(
                    "the default of {} is a valid value; a test checks it",
                    key.path
                );
            };
            values.entry(key.path).or_insert(Effective {
                value,
                source: Source::Default,
            });
        }
    }
    values.entry("paths.data").or_insert(Effective {
        value: Value::Path(default_data_dir(env)),
        source: Source::Default,
    });

    // Packages and the sockets live inside the data directory unless
    // placed elsewhere, so moving the data directory moves them too.
    let data = match values.get("paths.data") {
        Some(Effective {
            value: Value::Path(data),
            ..
        }) => data.clone(),
        _ => default_data_dir(env),
    };
    for (key, name) in [
        ("paths.packages", "packages"),
        ("viewer.tap_socket", "tap.sock"),
        ("viewer.frame_socket", "frames.sock"),
        ("capture.control_socket", "capture.sock"),
    ] {
        values.entry(key).or_insert(Effective {
            value: Value::Path(data.join(name)),
            source: Source::Default,
        });
    }
}

/// `$XDG_DATA_HOME/kapto`, else `~/.local/share/kapto`, else `kapto-data` in
/// the working directory.
fn default_data_dir(env: &dyn Fn(&str) -> Option<String>) -> PathBuf {
    let set = |name: &str| env(name).filter(|value| !value.is_empty());
    if let Some(data_home) = set("XDG_DATA_HOME") {
        Path::new(&data_home).join("kapto")
    } else if let Some(home) = set("HOME") {
        Path::new(&home).join(".local/share/kapto")
    } else {
        PathBuf::from("kapto-data")
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::net::Ipv4Addr;

    use super::*;
    use crate::destination::{Credentials, DEFAULT_LAYOUT};
    use crate::upload::{DeleteLocal, UploadWhen, Verification};

    fn env(vars: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let vars: HashMap<String, String> = vars
            .iter()
            .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
            .collect();
        move |name| vars.get(name).cloned()
    }

    fn file(contents: &str) -> ConfigFile {
        ConfigFile {
            path: PathBuf::from("/config/kapto.toml"),
            contents: contents.to_owned(),
        }
    }

    fn cli(key: &'static str, value: &str, flag: &str) -> CliOverride {
        CliOverride {
            key,
            value: value.to_owned(),
            flag: flag.to_owned(),
        }
    }

    fn resolve(
        file: Option<&ConfigFile>,
        env: &dyn Fn(&str) -> Option<String>,
        cli: &[CliOverride],
    ) -> Result<DeploymentConfig, Vec<KaptoError>> {
        DeploymentConfig::resolve(&Inputs { file, env, cli })
    }

    /// A file, environment, flags, and the port and source they should produce.
    type PrecedenceCase<'a> = (
        Option<&'a ConfigFile>,
        &'a dyn Fn(&str) -> Option<String>,
        &'a [CliOverride],
        u16,
        Source,
    );

    fn codes(errors: &[KaptoError]) -> Vec<ErrorCode> {
        errors.iter().map(KaptoError::code).collect()
    }

    fn line_source(line: usize) -> Source {
        Source::File {
            path: PathBuf::from("/config/kapto.toml"),
            line,
        }
    }

    #[test]
    fn defaults_apply_when_nothing_is_configured() {
        let config = resolve(None, &env(&[("HOME", "/home/u")]), &[]).unwrap();
        assert_eq!(config.server_bind.value, IpAddr::V4(Ipv4Addr::LOCALHOST));
        assert_eq!(config.server_port.value, 7700);
        assert_eq!(config.viewer_stream_port.value, 9877);
        assert_eq!(
            config.data_dir.value,
            Path::new("/home/u/.local/share/kapto")
        );
        assert_eq!(
            config.packages_dir.value,
            Path::new("/home/u/.local/share/kapto/packages")
        );
        assert!(
            config
                .entries()
                .iter()
                .all(|entry| entry.source == Source::Default)
        );
    }

    #[test]
    fn each_layer_overrides_the_one_before() {
        let file = file("[server]\nport = 7701\n");
        let file_env = env(&[]);
        let env_env = env(&[("KAPTO_SERVER_PORT", "7702")]);
        let flags = [cli("server.port", "7703", "--server-port")];

        let cases: [PrecedenceCase<'_>; 4] = [
            (None, &file_env, &[], 7700, Source::Default),
            (Some(&file), &file_env, &[], 7701, line_source(2)),
            (
                Some(&file),
                &env_env,
                &[],
                7702,
                Source::Env {
                    var: "KAPTO_SERVER_PORT".into(),
                },
            ),
            (
                Some(&file),
                &env_env,
                &flags,
                7703,
                Source::Cli {
                    flag: "--server-port".into(),
                },
            ),
        ];
        for (file, env, cli, port, source) in cases {
            let config = resolve(file, env, cli).unwrap();
            assert_eq!(config.server_port.value, port);
            assert_eq!(config.server_port.source, source);
        }
    }

    #[test]
    fn file_values_record_the_line_they_are_on() {
        let file = file(
            "# Kapto\n\n[server]\nbind = \"0.0.0.0\"\nport = 7701\n\n[paths]\npackages = \"/srv/packages\"\n",
        );
        let config = resolve(Some(&file), &env(&[]), &[]).unwrap();
        assert_eq!(config.server_bind.source, line_source(4));
        assert_eq!(config.server_port.source, line_source(5));
        assert_eq!(config.packages_dir.value, Path::new("/srv/packages"));
        assert_eq!(config.packages_dir.source, line_source(8));
    }

    #[test]
    fn inline_tables_and_dotted_keys_are_accepted() {
        let file = file("server = { port = 7701 }\nviewer.stream_port = 9091\n");
        let config = resolve(Some(&file), &env(&[]), &[]).unwrap();
        assert_eq!(config.server_port.value, 7701);
        assert_eq!(config.viewer_stream_port.value, 9091);
        assert_eq!(config.viewer_stream_port.source, line_source(2));
    }

    #[test]
    fn data_directory_follows_xdg_data_home() {
        let env = env(&[("XDG_DATA_HOME", "/srv/share"), ("HOME", "/home/u")]);
        let config = resolve(None, &env, &[]).unwrap();
        assert_eq!(config.data_dir.value, Path::new("/srv/share/kapto"));
    }

    #[test]
    fn packages_default_to_inside_the_configured_data_directory() {
        let env = env(&[("KAPTO_PATHS_DATA", "/data")]);
        let config = resolve(None, &env, &[]).unwrap();
        assert_eq!(config.packages_dir.value, Path::new("/data/packages"));
        assert_eq!(config.packages_dir.source, Source::Default);
    }

    #[test]
    fn the_viewer_socket_defaults_to_inside_the_data_directory_and_can_be_moved() {
        let inside = env(&[("KAPTO_PATHS_DATA", "/data")]);
        let config = resolve(None, &inside, &[]).unwrap();
        assert_eq!(config.tap_socket.value, Path::new("/data/tap.sock"));
        assert_eq!(config.tap_socket.source, Source::Default);
        let moved = env(&[("KAPTO_VIEWER_TAP_SOCKET", "/run/kapto/tap.sock")]);
        let config = resolve(None, &moved, &[]).unwrap();
        assert_eq!(config.tap_socket.value, Path::new("/run/kapto/tap.sock"));
    }

    #[test]
    fn the_frame_socket_defaults_to_inside_the_data_directory_and_can_be_moved() {
        let inside = env(&[("KAPTO_PATHS_DATA", "/data")]);
        let config = resolve(None, &inside, &[]).unwrap();
        assert_eq!(config.frame_socket.value, Path::new("/data/frames.sock"));
        assert_eq!(config.frame_socket.source, Source::Default);
        let file = file(
            "[viewer]
frame_socket = \"/run/kapto/frames.sock\"\n",
        );
        let moved = env(&[("KAPTO_VIEWER_FRAME_SOCKET", "/tmp/frames.sock")]);
        let config = resolve(Some(&file), &inside, &[]).unwrap();
        assert_eq!(
            config.frame_socket.value,
            Path::new("/run/kapto/frames.sock")
        );
        assert_eq!(config.frame_socket.source, line_source(2));
        let config = resolve(Some(&file), &moved, &[]).unwrap();
        assert_eq!(config.frame_socket.value, Path::new("/tmp/frames.sock"));
    }

    #[test]
    fn a_failed_pre_flight_check_refuses_the_start_unless_set_to_warn() {
        let config = resolve(None, &env(&[("HOME", "/home/u")]), &[]).unwrap();
        assert_eq!(config.preflight_failures.value, PreflightFailures::Refuse);
        let file = file("[capture]\npreflight = \"warn\"\n");
        let config = resolve(Some(&file), &env(&[("HOME", "/home/u")]), &[]).unwrap();
        assert_eq!(config.preflight_failures.value, PreflightFailures::Warn);
        let errors =
            resolve(None, &env(&[("KAPTO_CAPTURE_PREFLIGHT", "ignore")]), &[]).unwrap_err();
        assert_eq!(codes(&errors), [ErrorCode::ConfigInvalidEnvVar]);
    }

    #[test]
    fn recordings_are_compressed_with_lz4_unless_set_otherwise_by_file_env_or_flag() {
        let config = resolve(None, &env(&[("HOME", "/home/u")]), &[]).unwrap();
        assert_eq!(config.compression.value, Compression::Lz4);
        let file = file("[capture]\ncompression = \"zstd\"\n");
        let config = resolve(Some(&file), &env(&[("HOME", "/home/u")]), &[]).unwrap();
        assert_eq!(config.compression.value, Compression::Zstd);
        let config = resolve(
            Some(&file),
            &env(&[("HOME", "/home/u"), ("KAPTO_CAPTURE_COMPRESSION", "none")]),
            &[],
        )
        .unwrap();
        assert_eq!(config.compression.value, Compression::None);
        let errors =
            resolve(None, &env(&[("KAPTO_CAPTURE_COMPRESSION", "gzip")]), &[]).unwrap_err();
        assert_eq!(codes(&errors), [ErrorCode::ConfigInvalidEnvVar]);
    }

    #[test]
    fn the_control_socket_defaults_to_inside_the_data_directory_and_can_be_moved() {
        let inside = env(&[("KAPTO_PATHS_DATA", "/data")]);
        let config = resolve(None, &inside, &[]).unwrap();
        assert_eq!(config.control_socket.value, Path::new("/data/capture.sock"));
        assert_eq!(config.control_socket.source, Source::Default);
        let moved = env(&[("KAPTO_CAPTURE_CONTROL_SOCKET", "/run/kapto/capture.sock")]);
        let config = resolve(None, &moved, &[]).unwrap();
        assert_eq!(
            config.control_socket.value,
            Path::new("/run/kapto/capture.sock")
        );
    }

    #[test]
    fn an_explicit_packages_directory_is_kept() {
        let env = env(&[
            ("KAPTO_PATHS_DATA", "/data"),
            ("KAPTO_PATHS_PACKAGES", "/packages"),
        ]);
        let config = resolve(None, &env, &[]).unwrap();
        assert_eq!(config.packages_dir.value, Path::new("/packages"));
    }

    #[test]
    fn unrelated_kapto_environment_variables_are_ignored() {
        let env = env(&[
            ("KAPTO_URDF", "/robot/robot.urdf"),
            ("KAPTO_SERVER_PROT", "1"),
        ]);
        assert!(resolve(None, &env, &[]).is_ok());
    }

    #[test]
    fn a_misspelt_setting_is_reported_with_its_line_and_a_suggestion() {
        let file = file("[server]\nprot = 7701\n");
        let errors = resolve(Some(&file), &env(&[]), &[]).unwrap_err();
        assert_eq!(
            errors[0].report(),
            "error[config.unknown_key]: Unknown configuration key: `server.prot` (did you mean `server.port`?)\n  \
             file: /config/kapto.toml\n  \
             line: 2\n  \
             remedy: Remove the key or correct its spelling. `kapto config show` lists every valid key."
        );
    }

    #[test]
    fn an_unknown_section_is_reported_with_a_suggestion() {
        let file = file("[serer]\nport = 7701\n");
        let errors = resolve(Some(&file), &env(&[]), &[]).unwrap_err();
        assert_eq!(codes(&errors), [ErrorCode::ConfigUnknownKey]);
        assert!(
            errors[0]
                .message()
                .contains("`serer` (did you mean `server`?)")
        );
    }

    #[test]
    fn invalid_toml_is_reported_with_line_and_column() {
        let file = file("[server]\nport = \n");
        let errors = resolve(Some(&file), &env(&[]), &[]).unwrap_err();
        assert_eq!(codes(&errors), [ErrorCode::ConfigInvalidSyntax]);
        let context = errors[0].context_entries();
        assert!(context.contains(&("line", "2".to_owned())), "{context:?}");
        assert!(context.iter().any(|(key, _)| *key == "column"));
    }

    #[test]
    fn values_of_the_wrong_type_or_range_are_rejected() {
        for (contents, expected) in [
            (
                "[server]\nport = \"7701\"\n",
                "`server.port` must be an integer from 1 to 65535",
            ),
            (
                "[server]\nport = 0\n",
                "`server.port` must be an integer from 1 to 65535",
            ),
            (
                "[server]\nport = 70000\n",
                "`server.port` must be an integer from 1 to 65535",
            ),
            (
                "[server]\nbind = \"localhost\"\n",
                "`server.bind` must be an IP address",
            ),
            (
                "[capture]\ncompression = \"gzip\"\n",
                "`capture.compression` must be one of",
            ),
            (
                "[paths]\ndata = \"\"\n",
                "`paths.data` must be a non-empty path",
            ),
            ("server = 1\n", "`server` must be a [server] table"),
        ] {
            let errors = resolve(Some(&file(contents)), &env(&[]), &[]).unwrap_err();
            assert_eq!(
                codes(&errors),
                [ErrorCode::ConfigInvalidValue],
                "{contents}"
            );
            assert!(
                errors[0].message().contains(expected),
                "{}",
                errors[0].message()
            );
        }
    }

    #[test]
    fn uploads_default_to_every_finished_episode_with_the_marker_and_no_bandwidth_cap() {
        let upload = resolve(None, &env(&[]), &[]).unwrap().upload;
        assert_eq!(upload.when.value, UploadWhen::Finalised);
        assert_eq!(upload.delete_local.value, DeleteLocal::WhenDiskLow);
        assert_eq!(upload.verify.value, Verification::ServerChecksum);
        assert!(upload.marker.value && upload.pause_while_recording.value);
        assert_eq!(
            (upload.part_size_mib.value, upload.bandwidth_mib.value),
            (16, 0)
        );
        assert_eq!(upload.backoff_initial.value, Duration::from_millis(500));
    }

    #[test]
    fn an_upload_setting_in_the_file_is_overridden_by_its_environment_variable() {
        let file = file("[upload]\nwhen = \"accepted\"\nbackoff_max = 30\nbandwidth = 5\n");
        let only_file = resolve(Some(&file), &env(&[]), &[]).unwrap().upload;
        assert_eq!(only_file.when.value, UploadWhen::Accepted);
        assert_eq!(only_file.when.source, line_source(2));
        assert_eq!(only_file.backoff_max.value, Duration::from_secs(30));
        assert_eq!(only_file.bandwidth_mib.value, 5);

        let env = env(&[
            ("KAPTO_UPLOAD_WHEN", "never"),
            ("KAPTO_UPLOAD_BANDWIDTH", "0"),
        ]);
        let upload = resolve(Some(&file), &env, &[]).unwrap().upload;
        assert_eq!(upload.when.value, UploadWhen::Never);
        assert_eq!(
            upload.when.source,
            Source::Env {
                var: "KAPTO_UPLOAD_WHEN".to_owned()
            }
        );
        assert_eq!(upload.bandwidth_mib.value, 0);
        assert_eq!(upload.backoff_max.source, line_source(3));
    }

    #[test]
    fn an_upload_setting_out_of_its_range_is_refused_saying_what_is_accepted() {
        for (contents, expected) in [
            (
                "[upload]\nwhen = \"finalized\"\n",
                "`upload.when` must be one of \"never\"",
            ),
            (
                "[upload]\npart_size = 0\n",
                "`upload.part_size` must be a whole number of MiB",
            ),
            (
                "[upload]\nbandwidth = 100001\n",
                "`upload.bandwidth` must be a whole number of MiB/s from 0",
            ),
            (
                "[upload]\nverify = \"read-back\"\n",
                "`upload.verify` must be one of",
            ),
            (
                "[upload]\nmax_in_flight = 0\n",
                "`upload.max_in_flight` must be a whole number from 1",
            ),
        ] {
            let errors = resolve(Some(&file(contents)), &env(&[]), &[]).unwrap_err();
            assert_eq!(
                codes(&errors),
                [ErrorCode::ConfigInvalidValue],
                "{contents}"
            );
            assert!(
                errors[0].message().contains(expected),
                "{}",
                errors[0].message()
            );
        }
    }

    #[test]
    fn a_destination_starts_from_the_upload_settings_and_overrides_its_own() {
        let file = file(
            "[upload]\nbackoff_max = 120\nbandwidth = 5\n\
             [[upload.destination]]\nname = \"site_nas\"\nurl = \"s3://robot-data\"\n\
             prefix = \"fleet-a/robot-7\"\nendpoint = \"http://nas.local:9000\"\n\
             credentials = { env = \"SITE_NAS\" }\nbackoff_max = 30\nwhen = \"accepted\"\n\
             [[upload.destination]]\nname = \"cloud\"\nurl = \"gs://archive\"\n",
        );
        let env = env(&[("KAPTO_UPLOAD_WHEN", "never")]);
        let config = resolve(Some(&file), &env, &[]).unwrap();
        let [nas, cloud] = config.upload_destinations.as_slice() else {
            panic!("{:?}", config.upload_destinations);
        };
        assert_eq!(
            (nas.name.as_str(), nas.url.as_str()),
            ("site_nas", "s3://robot-data")
        );
        assert_eq!(nas.prefix, "fleet-a/robot-7");
        assert_eq!(
            nas.credentials,
            Some(Credentials::Env("SITE_NAS".to_owned()))
        );
        assert_eq!(nas.settings.backoff_max.value, Duration::from_secs(30));
        assert_eq!(nas.settings.backoff_max.source, line_source(10));
        // The destination's own line wins over the environment variable,
        // which sets what every other destination starts from.
        assert_eq!(nas.settings.when.value, UploadWhen::Accepted);
        assert_eq!(nas.settings.bandwidth_mib.value, 5);
        assert_eq!(cloud.settings.when.value, UploadWhen::Never);
        assert_eq!(cloud.settings.backoff_max.value, Duration::from_secs(120));
        assert_eq!(
            (
                cloud.prefix.as_str(),
                cloud.layout.as_str(),
                &cloud.credentials
            ),
            ("", DEFAULT_LAYOUT, &None)
        );
        assert_eq!(config.upload.when.value, UploadWhen::Never);
    }

    #[test]
    fn a_layout_may_split_the_date_into_its_parts() {
        let contents = "[[upload.destination]]\nname = \"n\"\nurl = \"s3://b\"\n\
                        layout = \"year={year}/month={month}/{day}/{hour}/{episode}_{sha8}\"\n";
        let config = resolve(Some(&file(contents)), &env(&[]), &[]).unwrap();
        assert_eq!(
            config.upload_destinations[0].layout,
            "year={year}/month={month}/{day}/{hour}/{episode}_{sha8}"
        );
    }

    #[test]
    fn a_secret_written_in_a_destination_is_refused_without_repeating_it() {
        let contents = "[[upload.destination]]\nname = \"nas\"\nurl = \"s3://b\"\n\
                        secret_access_key = \"abc123\"\n";
        let errors = resolve(Some(&file(contents)), &env(&[]), &[]).unwrap_err();
        assert_eq!(codes(&errors), [ErrorCode::ConfigSecretInline]);
        assert!(
            !errors[0].message().contains("abc123"),
            "{}",
            errors[0].message()
        );
        let pasted = "[[upload.destination]]\nname = \"nas\"\nurl = \"s3://b\"\n\
                      credentials = { env = \"wJalrXUtnFEMI/K7MDENG\" }\n";
        let errors = resolve(Some(&file(pasted)), &env(&[]), &[]).unwrap_err();
        assert_eq!(codes(&errors), [ErrorCode::ConfigInvalidValue]);
        assert!(
            !errors[0].message().contains("wJa"),
            "{}",
            errors[0].message()
        );
    }

    #[test]
    fn every_mistake_in_a_destination_is_reported_with_its_line() {
        for (entry, expected) in [
            (
                "name = \"Site NAS\"\nurl = \"s3://b\"",
                "a destination needs a `name`",
            ),
            ("name = \"n\"", "`url` is ``"),
            (
                "name = \"n\"\nurl = \"ftp://b\"",
                "with one of the schemes s3://",
            ),
            (
                "name = \"n\"\nurl = \"s3://b/fleet\"",
                "as `prefix = \"fleet\"`",
            ),
            (
                "name = \"n\"\nurl = \"s3://b\"\nprefix = \"a//b\"",
                "`prefix` is `a//b`",
            ),
            (
                "name = \"n\"\nurl = \"s3://b\"\nlayout = \"{robot}\"",
                "it needs `{episode}`",
            ),
            (
                "name = \"n\"\nurl = \"s3://b\"\nlayout = \"{who}/{episode}\"",
                "it may hold {robot}",
            ),
            (
                "name = \"n\"\nurl = \"s3://b\"\nlayout = \"{date:%Y}/{episode}\"",
                "it may hold {robot}",
            ),
            (
                "name = \"n\"\nurl = \"s3://b\"\nendpoint = \"nas:9000\"",
                "starting with http://",
            ),
            (
                "name = \"n\"\nurl = \"s3://b\"\ncredentials = \"x\"",
                "`credentials` says where",
            ),
            (
                "name = \"n\"\nurl = \"s3://b\"\nbackoff_max = 0",
                "`backoff_max` of destination `n` must be",
            ),
        ] {
            let contents = format!("[[upload.destination]]\n{entry}\n");
            let errors = resolve(Some(&file(&contents)), &env(&[]), &[]).unwrap_err();
            assert_eq!(codes(&errors), [ErrorCode::ConfigInvalidValue], "{entry}");
            assert!(
                errors[0].message().contains(expected),
                "{entry}: {}",
                errors[0].message()
            );
        }
        let twice = "[[upload.destination]]\nname = \"n\"\nurl = \"s3://a\"\n\
                     [[upload.destination]]\nname = \"n\"\nurl = \"s3://b\"\n";
        let errors = resolve(Some(&file(twice)), &env(&[]), &[]).unwrap_err();
        assert!(
            errors[0]
                .message()
                .contains("two destinations are named `n`")
        );
        let misspelt = "[[upload.destination]]\nname = \"n\"\nurl = \"s3://b\"\nregoin = \"eu\"\n";
        let errors = resolve(Some(&file(misspelt)), &env(&[]), &[]).unwrap_err();
        assert_eq!(codes(&errors), [ErrorCode::ConfigUnknownKey]);
        assert!(
            errors[0].message().contains("did you mean `region`"),
            "{}",
            errors[0].message()
        );
    }

    #[test]
    fn an_invalid_environment_variable_names_the_variable() {
        let env = env(&[("KAPTO_SERVER_PORT", "abc")]);
        let errors = resolve(None, &env, &[]).unwrap_err();
        assert_eq!(codes(&errors), [ErrorCode::ConfigInvalidEnvVar]);
        assert_eq!(
            errors[0].message(),
            "Environment variable has an invalid value: KAPTO_SERVER_PORT=\"abc\" must be an integer from 1 to 65535"
        );
    }

    #[test]
    fn an_invalid_flag_value_names_the_flag() {
        let errors =
            resolve(None, &env(&[]), &[cli("server.port", "x", "--server-port")]).unwrap_err();
        assert_eq!(codes(&errors), [ErrorCode::ConfigInvalidValue]);
        assert!(errors[0].message().contains("--server-port \"x\" must be"));
    }

    #[test]
    fn every_problem_is_reported_at_once() {
        let file = file("[server]\nprot = 1\nport = 0\n[capture]\ncompression = \"gzip\"\n");
        let env = env(&[("KAPTO_VIEWER_STREAM_PORT", "-1")]);
        let errors = resolve(Some(&file), &env, &[]).unwrap_err();
        assert_eq!(
            codes(&errors),
            [
                ErrorCode::ConfigUnknownKey,
                ErrorCode::ConfigInvalidValue,
                ErrorCode::ConfigInvalidValue,
                ErrorCode::ConfigInvalidEnvVar,
            ]
        );
    }

    #[test]
    fn two_listeners_on_one_port_are_rejected_naming_both_sources() {
        let file = file("[viewer]\nstream_port = 7700\n");
        let errors = resolve(Some(&file), &env(&[]), &[]).unwrap_err();
        assert_eq!(codes(&errors), [ErrorCode::ConfigPortConflict]);
        assert_eq!(
            errors[0].report(),
            "error[config.port_conflict]: Two listeners are configured on the same port: \
             server.port and viewer.stream_port are both 7700\n  \
             server.port: default\n  \
             viewer.stream_port: /config/kapto.toml:2\n  \
             remedy: Give each listener its own port. Kapto never picks a different port on its own."
        );
    }

    #[test]
    fn the_viewer_stream_port_comes_from_the_file_the_environment_or_a_flag_and_differs_from_the_others()
     {
        assert_eq!(
            resolve(None, &env(&[]), &[])
                .unwrap()
                .viewer_stream_port
                .value,
            9877
        );
        let file = file("[viewer]\nstream_port = 9900\n");
        let config = resolve(
            Some(&file),
            &env(&[("KAPTO_VIEWER_STREAM_PORT", "9901")]),
            &[],
        )
        .unwrap();
        assert_eq!(config.viewer_stream_port.value, 9901);
        let errors = resolve(Some(&file), &env(&[("KAPTO_SERVER_PORT", "9900")]), &[]).unwrap_err();
        assert_eq!(codes(&errors), [ErrorCode::ConfigPortConflict]);
        assert!(
            errors[0]
                .report()
                .contains("server.port and viewer.stream_port are both 9900")
        );
    }

    #[test]
    fn how_far_back_the_live_view_looks_is_ten_minutes_unless_set() {
        assert_eq!(
            resolve(None, &env(&[]), &[])
                .unwrap()
                .viewer_history_age
                .value,
            Duration::from_secs(600)
        );
        let file = file("[viewer]\nhistory_age = 120\n");
        let config = resolve(Some(&file), &env(&[]), &[]).unwrap();
        assert_eq!(config.viewer_history_age.value, Duration::from_secs(120));
        let config = resolve(
            Some(&file),
            &env(&[("KAPTO_VIEWER_HISTORY_AGE", "30")]),
            &[],
        )
        .unwrap();
        assert_eq!(config.viewer_history_age.value, Duration::from_secs(30));
    }

    #[test]
    fn a_port_conflict_is_reported_alongside_other_problems() {
        let file = file("[server]\nprot = 1\n[viewer]\nstream_port = 7700\n");
        let errors = resolve(Some(&file), &env(&[]), &[]).unwrap_err();
        assert_eq!(
            codes(&errors),
            [ErrorCode::ConfigUnknownKey, ErrorCode::ConfigPortConflict]
        );
    }

    #[test]
    fn an_invalid_port_does_not_cause_a_conflict_report_with_the_fallback() {
        // server.port is invalid, so its default (7700) must not be compared
        // with the viewer's stream port.
        let file = file("[server]\nport = 0\n[viewer]\nstream_port = 7700\n");
        let errors = resolve(Some(&file), &env(&[]), &[]).unwrap_err();
        assert_eq!(codes(&errors), [ErrorCode::ConfigInvalidValue]);
    }

    #[test]
    fn entries_list_every_setting_in_documentation_order() {
        let config = resolve(None, &env(&[("HOME", "/home/u")]), &[]).unwrap();
        let keys: Vec<&str> = config
            .entries()
            .iter()
            .map(|entry| entry.key.path)
            .collect();
        let expected: Vec<&str> = DEPLOYMENT_KEYS.iter().map(|key| key.path).collect();
        assert_eq!(keys, expected);
    }

    #[test]
    fn times_rates_and_sizes_have_defaults_and_follow_every_layer() {
        let config = resolve(None, &env(&[]), &[]).unwrap();
        assert_eq!(config.restart_delay.value, Duration::from_secs(1));
        assert_eq!(config.restart_max_delay.value, Duration::from_secs(30));
        assert_eq!(config.stable_after.value, Duration::from_secs(60));
        assert_eq!(config.capture_stop_grace.value, Duration::from_secs(30));
        assert_eq!(config.disk_warn_below_mib.value, 10_240);
        assert_eq!(config.disk_stop_below_mib.value, 2_048);
        assert_eq!(config.pre_roll_max.value, 64);
        assert_eq!(config.preflight_window.value, Duration::from_secs(5));
        assert_eq!(config.preflight_disk_time.value, Duration::from_secs(600));
        assert_eq!(config.viewer_stop_grace.value, Duration::from_secs(5));
        assert_eq!(config.control_timeout.value, Duration::from_secs(60));
        assert_eq!(config.viewer_display_rate.value, 60);
        assert_eq!(config.viewer_camera_max_fps.value, 30);
        assert_eq!(config.viewer_history_mib.value, 64);
        assert_eq!(config.replay_cache_size_mib.value, 2048);
        assert_eq!(config.replay_cache_age_days.value, 7);
        assert_eq!(config.worker_max_attempts.value, 3);
        assert!(config.worker_pause_while_recording.value);
        assert!(!config.attach_meshes.value);

        let file = file(
            "[supervisor]\nrestart_delay = 0.5\nstable_after = 10\n\
             [viewer]\ndisplay_rate = 30\ncamera_max_fps = 15\nhistory = 256\nreplay_cache_age = 30\n\
             [capture]\ncontrol_timeout = 20\nattach_meshes = true\n[worker]\nmax_attempts = 5\npause_while_recording = false\n",
        );
        let vars = env(&[
            ("KAPTO_CAPTURE_CONTROL_TIMEOUT", "2.5"),
            ("KAPTO_VIEWER_REPLAY_CACHE_AGE", "14"),
            ("KAPTO_VIEWER_CAMERA_MAX_FPS", "10"),
        ]);
        let flags = [
            cli("supervisor.stable_after", "15", "--supervisor-stable-after"),
            cli(
                "viewer.replay_cache_size",
                "512",
                "--viewer-replay-cache-size",
            ),
        ];
        let config = resolve(Some(&file), &vars, &flags).unwrap();
        assert_eq!(config.restart_delay.value, Duration::from_millis(500));
        assert_eq!(config.restart_delay.source, line_source(2));
        assert_eq!(config.viewer_display_rate.value, 30);
        // The environment over the file.
        assert_eq!(config.viewer_camera_max_fps.value, 10);
        assert_eq!(config.viewer_history_mib.value, 256);
        assert_eq!(config.replay_cache_age_days.value, 14);
        assert_eq!(config.replay_cache_size_mib.value, 512);
        assert_eq!(config.worker_max_attempts.value, 5);
        assert!(!config.worker_pause_while_recording.value);
        assert!(config.attach_meshes.value);
        assert_eq!(config.control_timeout.value, Duration::from_millis(2500));
        assert_eq!(
            config.control_timeout.source,
            Source::Env {
                var: "KAPTO_CAPTURE_CONTROL_TIMEOUT".into()
            }
        );
        assert_eq!(config.stable_after.value, Duration::from_secs(15));
        let listed = config
            .entries()
            .into_iter()
            .find(|entry| entry.key.path == "supervisor.restart_delay")
            .unwrap();
        assert_eq!(listed.value, "0.5");
    }

    #[test]
    fn times_rates_and_sizes_out_of_range_are_rejected() {
        let file = file(
            "[supervisor]\nrestart_delay = 0\n[viewer]\ndisplay_rate = 0\ncamera_max_fps = 0\nhistory = 1.5\n\
             stop_grace = \"5\"\nreplay_cache_age = 0\n[worker]\nmax_attempts = 0\npause_while_recording = \"no\"\n",
        );
        let vars = env(&[("KAPTO_CAPTURE_CONTROL_TIMEOUT", "5s")]);
        let errors = resolve(Some(&file), &vars, &[]).unwrap_err();
        assert_eq!(
            codes(&errors),
            [
                ErrorCode::ConfigInvalidValue,
                ErrorCode::ConfigInvalidValue,
                ErrorCode::ConfigInvalidValue,
                ErrorCode::ConfigInvalidValue,
                ErrorCode::ConfigInvalidValue,
                ErrorCode::ConfigInvalidValue,
                ErrorCode::ConfigInvalidValue,
                ErrorCode::ConfigInvalidValue,
                ErrorCode::ConfigInvalidEnvVar,
            ]
        );
        assert!(
            errors[0].message().contains("above 0 and at most 3600"),
            "{}",
            errors[0].message()
        );
    }
}
