// SPDX-License-Identifier: Apache-2.0

//! The `kapto` command-line tool.
//!
//! The binary's `main` only calls `run`; the definitions live in this library
//! so tooling (such as the reference documentation generator) can read them.
//! Such tools build it without the `run` feature, which leaves out `run` and
//! the live view.

// Without `run`, the commands are only described, never called.
#![cfg_attr(not(feature = "run"), allow(dead_code, unused_imports))]

mod capture;
mod config;
mod decode;
mod demo;
mod episode;
mod export;
#[cfg(feature = "run")]
mod live_view;
mod overview;
mod own_events;
mod package;
mod parent;
mod pcap;
mod preflight;
mod record;
mod serve;
mod session;
mod showcase;
mod station;
mod topics;
mod upload;
mod viewer_tap;
mod viz_bridge;
mod worker;

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Args, CommandFactory, Parser, Subcommand};
use kapto_config::CliOverride;

#[derive(Parser)]
#[command(
    name = "kapto",
    version,
    about = "Records, visualises and selects robot data on the edge."
)]
struct Cli {
    #[command(flatten)]
    settings: SettingFlags,
    #[command(subcommand)]
    command: Command,
}

// Flags that override settings from the configuration file and environment.
// Values are passed on as text so they are checked, and reported, exactly like
// values from the file or the environment. (A plain comment: clap would show a
// doc comment here as the description of the whole program.)
#[derive(Args, Clone, Default)]
struct SettingFlags {
    /// Configuration file to use instead of the default location.
    #[arg(long, global = true, value_name = "FILE")]
    config: Option<PathBuf>,
    /// Address the API and web UI listen on (server.bind).
    #[arg(long, global = true, value_name = "ADDRESS")]
    bind: Option<String>,
    /// Port of the API and web UI (server.port).
    #[arg(long, global = true, value_name = "PORT")]
    port: Option<String>,
    /// Port the viewer bridge streams poses and plotted values on (`viewer.stream_port`).
    #[arg(long, global = true, value_name = "PORT")]
    viewer_stream_port: Option<String>,
    /// Unix socket live samples are sent to, for the viewer (`viewer.tap_socket`).
    #[arg(long, global = true, value_name = "PATH")]
    tap_socket: Option<String>,
    /// Unix socket the capture process takes its orders on (`capture.control_socket`).
    #[arg(long, global = true, value_name = "PATH")]
    control_socket: Option<String>,
    /// Where recordings and runtime state are stored (paths.data).
    #[arg(long, global = true, value_name = "DIR")]
    data_dir: Option<String>,
    /// Where robot packages are stored (paths.packages).
    #[arg(long, global = true, value_name = "DIR")]
    packages_dir: Option<String>,
    /// How much to log: error, warn, info, debug or trace (log.level).
    #[arg(long, global = true, value_name = "LEVEL")]
    log_level: Option<String>,
    /// How many times a second the live view moves the robot (`viewer.display_rate`).
    #[arg(long, global = true, value_name = "HZ")]
    viewer_display_rate: Option<String>,
    /// MiB of recent live data the viewer bridge keeps for later viewers (viewer.history).
    #[arg(long, global = true, value_name = "MIB")]
    viewer_history: Option<String>,
    /// Seconds the live view can look back over (`viewer.history_age`).
    #[arg(long, global = true, value_name = "SECONDS")]
    viewer_history_age: Option<String>,
    /// Seconds the viewer bridge may take to stop before it is killed (`viewer.stop_grace`).
    #[arg(long, global = true, value_name = "SECONDS")]
    viewer_stop_grace: Option<String>,
    /// MiB replays of recorded episodes may take in the cache (`viewer.replay_cache_size`).
    #[arg(long, global = true, value_name = "MIB")]
    replay_cache_size: Option<String>,
    /// Days unopened after which a replay leaves the cache (`viewer.replay_cache_age`).
    #[arg(long, global = true, value_name = "DAYS")]
    replay_cache_age: Option<String>,
    /// Seconds capture keeps recording after serve has gone (`capture.control_timeout`).
    #[arg(long, global = true, value_name = "SECONDS")]
    control_timeout: Option<String>,
    /// Seconds the capture process may take to stop before it is killed (`capture.stop_grace`).
    #[arg(long, global = true, value_name = "SECONDS")]
    capture_stop_grace: Option<String>,
    /// MiB free below which Kapto warns the disk is running low (`capture.disk_warn_below`).
    #[arg(long, global = true, value_name = "MIB")]
    disk_warn_below: Option<String>,
    /// MiB free below which recording stops and none starts (`capture.disk_stop_below`).
    #[arg(long, global = true, value_name = "MIB")]
    disk_stop_below: Option<String>,
    /// Seconds pre-flight watches the streams (`capture.preflight_window`).
    #[arg(long, global = true, value_name = "SECONDS")]
    preflight_window: Option<String>,
    /// Seconds of recording pre-flight checks the disk has room for (`capture.preflight_disk_time`).
    #[arg(long, global = true, value_name = "SECONDS")]
    preflight_disk_time: Option<String>,
    /// What a failed pre-flight check does to a start: refuse or warn (`capture.preflight`).
    #[arg(long, global = true, value_name = "POLICY")]
    preflight: Option<String>,
    /// Whether recordings also carry the robot package's meshes (`capture.attach_meshes`).
    #[arg(long, global = true, value_name = "BOOL")]
    attach_meshes: Option<String>,
    /// How recordings are compressed: lz4, zstd or none (`capture.compression`).
    #[arg(long, global = true, value_name = "COMPRESSION")]
    capture_compression: Option<String>,
    /// Seconds before a process that exited is started again (`supervisor.restart_delay`).
    #[arg(long, global = true, value_name = "SECONDS")]
    restart_delay: Option<String>,
    /// The longest wait before a process is started again (`supervisor.restart_max_delay`).
    #[arg(long, global = true, value_name = "SECONDS")]
    restart_max_delay: Option<String>,
    /// Seconds a process must run for its exit to count as a new failure (`supervisor.stable_after`).
    #[arg(long, global = true, value_name = "SECONDS")]
    restart_stable_after: Option<String>,
    /// How many times the worker may start a job (`worker.max_attempts`).
    #[arg(long, global = true, value_name = "COUNT")]
    worker_max_attempts: Option<String>,
    /// Whether the worker waits while an episode is recorded (`worker.pause_while_recording`).
    #[arg(long, global = true, value_name = "BOOL")]
    worker_pause_while_recording: Option<String>,
    /// Which episodes are uploaded by themselves (`upload.when`).
    #[arg(long, global = true, value_name = "WHEN")]
    upload_when: Option<String>,
    /// Whether uploads wait while an episode is recorded (`upload.pause_while_recording`).
    #[arg(long, global = true, value_name = "BOOL")]
    upload_pause_while_recording: Option<String>,
    /// What happens to an episode's local copy once uploaded (`upload.delete_local`).
    #[arg(long, global = true, value_name = "WHEN")]
    upload_delete_local: Option<String>,
    /// On how many destinations an episode is verified before its local copy may go (`upload.min_verified_destinations`).
    #[arg(long, global = true, value_name = "COUNT")]
    upload_min_verified_destinations: Option<String>,
    /// How uploaded files are checked (`upload.verify`).
    #[arg(long, global = true, value_name = "HOW")]
    upload_verify: Option<String>,
    /// Whether the completeness marker is written (`upload.marker`).
    #[arg(long, global = true, value_name = "BOOL")]
    upload_marker: Option<String>,
    /// The size of a file's parts (`upload.part_size`).
    #[arg(long, global = true, value_name = "MIB")]
    upload_part_size: Option<String>,
    /// How many parts of a file go up at once (`upload.parallel_parts`).
    #[arg(long, global = true, value_name = "COUNT")]
    upload_parallel_parts: Option<String>,
    /// How many files go up at once (`upload.parallel_files`).
    #[arg(long, global = true, value_name = "COUNT")]
    upload_parallel_files: Option<String>,
    /// The most requests in flight at once (`upload.max_in_flight`).
    #[arg(long, global = true, value_name = "COUNT")]
    upload_max_in_flight: Option<String>,
    /// The bandwidth cap; 0 for none (`upload.bandwidth`).
    #[arg(long, global = true, value_name = "MIB_PER_S")]
    upload_bandwidth: Option<String>,
    /// How many times a file is tried (`upload.max_attempts`).
    #[arg(long, global = true, value_name = "COUNT")]
    upload_max_attempts: Option<String>,
    /// How long before a failed upload is tried again (`upload.retry_failed_after`).
    #[arg(long, global = true, value_name = "SECONDS")]
    upload_retry_failed_after: Option<String>,
    /// The first wait before a request is tried again (`upload.backoff_initial`).
    #[arg(long, global = true, value_name = "SECONDS")]
    upload_backoff_initial: Option<String>,
    /// The longest wait before a request is tried again (`upload.backoff_max`).
    #[arg(long, global = true, value_name = "SECONDS")]
    upload_backoff_max: Option<String>,
    /// How long connecting may take (`upload.timeout_connect`).
    #[arg(long, global = true, value_name = "SECONDS")]
    upload_timeout_connect: Option<String>,
    /// How long one request may take (`upload.timeout_request`).
    #[arg(long, global = true, value_name = "SECONDS")]
    upload_timeout_request: Option<String>,
}

/// The flag that sets each deployment setting, as `(setting, flag)`.
pub const SETTING_FLAGS: &[(&str, &str)] = &[
    ("server.bind", "--bind"),
    ("server.port", "--port"),
    ("viewer.stream_port", "--viewer-stream-port"),
    ("viewer.tap_socket", "--tap-socket"),
    ("capture.control_socket", "--control-socket"),
    ("paths.data", "--data-dir"),
    ("paths.packages", "--packages-dir"),
    ("log.level", "--log-level"),
    ("viewer.display_rate", "--viewer-display-rate"),
    ("viewer.history", "--viewer-history"),
    ("viewer.history_age", "--viewer-history-age"),
    ("viewer.stop_grace", "--viewer-stop-grace"),
    ("viewer.replay_cache_size", "--replay-cache-size"),
    ("viewer.replay_cache_age", "--replay-cache-age"),
    ("capture.control_timeout", "--control-timeout"),
    ("capture.stop_grace", "--capture-stop-grace"),
    ("capture.disk_warn_below", "--disk-warn-below"),
    ("capture.disk_stop_below", "--disk-stop-below"),
    ("capture.preflight_window", "--preflight-window"),
    ("capture.preflight_disk_time", "--preflight-disk-time"),
    ("capture.preflight", "--preflight"),
    ("capture.attach_meshes", "--attach-meshes"),
    ("capture.compression", "--capture-compression"),
    ("supervisor.restart_delay", "--restart-delay"),
    ("supervisor.restart_max_delay", "--restart-max-delay"),
    ("supervisor.stable_after", "--restart-stable-after"),
    ("worker.max_attempts", "--worker-max-attempts"),
    (
        "worker.pause_while_recording",
        "--worker-pause-while-recording",
    ),
    ("upload.when", "--upload-when"),
    (
        "upload.pause_while_recording",
        "--upload-pause-while-recording",
    ),
    ("upload.delete_local", "--upload-delete-local"),
    (
        "upload.min_verified_destinations",
        "--upload-min-verified-destinations",
    ),
    ("upload.verify", "--upload-verify"),
    ("upload.marker", "--upload-marker"),
    ("upload.part_size", "--upload-part-size"),
    ("upload.parallel_parts", "--upload-parallel-parts"),
    ("upload.parallel_files", "--upload-parallel-files"),
    ("upload.max_in_flight", "--upload-max-in-flight"),
    ("upload.bandwidth", "--upload-bandwidth"),
    ("upload.max_attempts", "--upload-max-attempts"),
    ("upload.retry_failed_after", "--upload-retry-failed-after"),
    ("upload.backoff_initial", "--upload-backoff-initial"),
    ("upload.backoff_max", "--upload-backoff-max"),
    ("upload.timeout_connect", "--upload-timeout-connect"),
    ("upload.timeout_request", "--upload-timeout-request"),
];

impl SettingFlags {
    /// The same flags as command-line arguments, for a child process to
    /// resolve the same settings.
    fn to_args(&self) -> Vec<std::ffi::OsString> {
        let mut args = Vec::new();
        if let Some(config) = &self.config {
            args.push("--config".into());
            args.push(config.clone().into_os_string());
        }
        for flag in self.overrides() {
            args.push(flag.flag.into());
            args.push(flag.value.into());
        }
        args
    }

    /// The flags that were given, as overrides for the settings they set.
    fn overrides(&self) -> Vec<CliOverride> {
        // Same order as SETTING_FLAGS.
        let values = [
            &self.bind,
            &self.port,
            &self.viewer_stream_port,
            &self.tap_socket,
            &self.control_socket,
            &self.data_dir,
            &self.packages_dir,
            &self.log_level,
            &self.viewer_display_rate,
            &self.viewer_history,
            &self.viewer_history_age,
            &self.viewer_stop_grace,
            &self.replay_cache_size,
            &self.replay_cache_age,
            &self.control_timeout,
            &self.capture_stop_grace,
            &self.disk_warn_below,
            &self.disk_stop_below,
            &self.preflight_window,
            &self.preflight_disk_time,
            &self.preflight,
            &self.attach_meshes,
            &self.capture_compression,
            &self.restart_delay,
            &self.restart_max_delay,
            &self.restart_stable_after,
            &self.worker_max_attempts,
            &self.worker_pause_while_recording,
            &self.upload_when,
            &self.upload_pause_while_recording,
            &self.upload_delete_local,
            &self.upload_min_verified_destinations,
            &self.upload_verify,
            &self.upload_marker,
            &self.upload_part_size,
            &self.upload_parallel_parts,
            &self.upload_parallel_files,
            &self.upload_max_in_flight,
            &self.upload_bandwidth,
            &self.upload_max_attempts,
            &self.upload_retry_failed_after,
            &self.upload_backoff_initial,
            &self.upload_backoff_max,
            &self.upload_timeout_connect,
            &self.upload_timeout_request,
        ];
        SETTING_FLAGS
            .iter()
            .zip(values)
            .filter_map(|(&(key, flag), value)| {
                value.as_ref().map(|value| CliOverride {
                    key,
                    value: value.clone(),
                    flag: flag.to_owned(),
                })
            })
            .collect()
    }
}

#[derive(Subcommand)]
enum Command {
    /// Inspect Kapto's configuration.
    #[command(subcommand)]
    Config(ConfigCommand),
    /// Record one episode from the UDP streams declared in a streams.toml,
    /// for a fixed time, into the data directory.
    Record {
        /// The streams.toml declaring each stream's UDP port and rate.
        #[arg(long, value_name = "FILE")]
        streams: PathBuf,
        /// The mapping.toml saying which decoded value drives which joint,
        /// carried by each recording when the streams aren't declared in a
        /// robot package, so its quality checks know the joints.
        #[arg(long, value_name = "FILE")]
        mapping: Option<PathBuf>,
        /// How long to record, in seconds.
        #[arg(long, value_name = "SECONDS", value_parser = clap::value_parser!(u32).range(1..=604_800))]
        duration: u32,
    },
    /// Receive every stream declared in a streams.toml for as long as it
    /// runs, feeding the live view, and record episodes when told to over
    /// the control socket; SIGINT or SIGTERM finalises the episode being
    /// recorded and stops.
    Capture {
        /// The streams.toml declaring each stream's input and rate.
        #[arg(long, value_name = "FILE")]
        streams: PathBuf,
        /// The mapping.toml saying which decoded value drives which joint,
        /// carried by each recording when the streams aren't declared in a
        /// robot package, so its quality checks know the joints.
        #[arg(long, value_name = "FILE")]
        mapping: Option<PathBuf>,
    },
    /// Start and stop episodes of a running `kapto capture`, or show what it
    /// is doing.
    #[command(subcommand)]
    Episode(episode::EpisodeCommand),
    /// List the robot packages Kapto finds, or show one: what identifies
    /// it and its files.
    #[command(subcommand)]
    Package(package::PackageCommand),
    /// List the session profiles in the configuration folder, or show one:
    /// with which robot, by whom, where and how episodes are recorded.
    #[command(subcommand)]
    Profile(session::ProfileCommand),
    /// List the tasks in the configuration folder, or show one: what the
    /// robot is asked to do.
    #[command(subcommand)]
    Task(session::TaskCommand),
    /// Show a recording's messages as named values, decoded with the layouts
    /// stored in it; streams without one are shown as raw bytes. With --pcap
    /// and --package, decode a packet capture with a package's streams.
    Decode(decode::DecodeArgs),
    /// Try Kapto without a robot: run it with the built-in sample arm and a
    /// synthetic robot sending its joint states, then open the address it
    /// prints.
    Demo(demo::DemoArgs),
    /// Show Kapto to people in one command: the demo, narrated step by step,
    /// recording ordinary episodes and then one in which the robot
    /// misbehaves, and saying what Kapto found and where to watch it.
    Showcase(showcase::ShowcaseArgs),
    /// Run Kapto: start the capture process, the viewer bridge and the
    /// worker, keep them running, serve the API, and stop everything on
    /// SIGINT or SIGTERM.
    Serve(serve::ServeArgs),
    /// Stream the robot moving live to the web UI's live view, from the
    /// samples a running capture process or recording sends; runs until
    /// interrupted.
    VizBridge(viz_bridge::VizBridgeArgs),
    /// Do the jobs `kapto serve` queues, such as checking each finalised
    /// episode's files against its manifest, one at a time; runs until
    /// SIGINT or SIGTERM. Serve starts it.
    Worker(worker::WorkerArgs),
    /// List the ROS 2 topics this machine's ROS 2 network has, with their
    /// message types, for declaring a ROS 2 robot's streams. Only the Kapto
    /// of the `kapto-ros2` image is built with ROS 2.
    Topics(topics::TopicsArgs),
    /// Write an episode in another tool's format: its ROS 2 topics as a
    /// rosbag2 bag that `ros2 bag play` plays back on the robot's topics.
    #[command(subcommand)]
    Export(export::ExportCommand),
    /// Work with the upload destinations in `[[upload.destination]]`:
    /// test that each can be written to as an upload would.
    #[command(subcommand)]
    Upload(upload::UploadCommand),
}

#[derive(Subcommand)]
enum ConfigCommand {
    /// Check the configuration and report every problem.
    Validate,
    /// Show every setting with its value and where the value came from.
    Show,
}

/// The full command-line definition: commands, flags and help texts.
#[must_use]
pub fn command() -> clap::Command {
    Cli::command()
}

/// Parses the process arguments and runs the chosen command.
#[cfg(feature = "run")]
#[must_use]
pub fn run() -> ExitCode {
    let cli = Cli::parse();
    match cli.command {
        Command::Config(ConfigCommand::Validate) => config::validate(&cli.settings),
        Command::Config(ConfigCommand::Show) => config::show(&cli.settings),
        Command::Decode(args) => decode::decode(&cli.settings, &args),
        Command::Capture { streams, mapping } => {
            capture::capture(&cli.settings, &streams, mapping.as_deref())
        }
        Command::Episode(command) => episode::episode(&cli.settings, &command),
        Command::Package(command) => package::package(&cli.settings, &command),
        Command::Profile(command) => session::profile(&cli.settings, &command),
        Command::Task(command) => session::task(&cli.settings, &command),
        Command::Demo(args) => demo::demo(&cli.settings, &args),
        Command::Showcase(args) => showcase::showcase(&cli.settings, &args),
        Command::Serve(args) => serve::serve(&cli.settings, &args),
        Command::VizBridge(args) => live_view::viz_bridge(&cli.settings, &args),
        Command::Worker(args) => worker::worker(&cli.settings, &args),
        Command::Topics(args) => topics::topics(&args),
        Command::Export(command) => export::export(&command),
        Command::Upload(command) => upload::upload(&cli.settings, &command),
        Command::Record {
            streams,
            mapping,
            duration,
        } => record::record(
            &cli.settings,
            &streams,
            mapping.as_deref(),
            std::time::Duration::from_secs(u64::from(duration)),
        ),
    }
}

#[cfg(test)]
mod tests {
    use kapto_config::DEPLOYMENT_KEYS;

    use super::*;

    #[test]
    fn command_line_definition_is_consistent() {
        Cli::command().debug_assert();
    }

    #[test]
    fn every_listed_flag_exists_on_the_command_line() {
        let command = command();
        for (setting, flag) in SETTING_FLAGS {
            let long = flag.trim_start_matches("--");
            assert!(
                command
                    .get_arguments()
                    .any(|arg| arg.get_long() == Some(long)),
                "{flag} for {setting} is not a flag of kapto"
            );
        }
    }

    #[test]
    fn every_flag_sets_the_setting_listed_for_it() {
        let all = SettingFlags {
            config: None,
            bind: Some("bind".into()),
            port: Some("port".into()),
            viewer_stream_port: Some("viewer-stream-port".into()),
            tap_socket: Some("tap-socket".into()),
            control_socket: Some("control-socket".into()),
            data_dir: Some("data-dir".into()),
            packages_dir: Some("packages-dir".into()),
            log_level: Some("log-level".into()),
            viewer_display_rate: Some("viewer-display-rate".into()),
            viewer_history: Some("viewer-history".into()),
            viewer_history_age: Some("viewer-history-age".into()),
            viewer_stop_grace: Some("viewer-stop-grace".into()),
            replay_cache_size: Some("replay-cache-size".into()),
            replay_cache_age: Some("replay-cache-age".into()),
            control_timeout: Some("control-timeout".into()),
            capture_stop_grace: Some("capture-stop-grace".into()),
            disk_warn_below: Some("disk-warn-below".into()),
            disk_stop_below: Some("disk-stop-below".into()),
            preflight_window: Some("preflight-window".into()),
            preflight_disk_time: Some("preflight-disk-time".into()),
            preflight: Some("preflight".into()),
            attach_meshes: Some("attach-meshes".into()),
            capture_compression: Some("capture-compression".into()),
            restart_delay: Some("restart-delay".into()),
            restart_max_delay: Some("restart-max-delay".into()),
            restart_stable_after: Some("restart-stable-after".into()),
            worker_max_attempts: Some("worker-max-attempts".into()),
            worker_pause_while_recording: Some("worker-pause-while-recording".into()),
            upload_when: Some("upload-when".into()),
            upload_pause_while_recording: Some("upload-pause-while-recording".into()),
            upload_delete_local: Some("upload-delete-local".into()),
            upload_min_verified_destinations: Some("upload-min-verified-destinations".into()),
            upload_verify: Some("upload-verify".into()),
            upload_marker: Some("upload-marker".into()),
            upload_part_size: Some("upload-part-size".into()),
            upload_parallel_parts: Some("upload-parallel-parts".into()),
            upload_parallel_files: Some("upload-parallel-files".into()),
            upload_max_in_flight: Some("upload-max-in-flight".into()),
            upload_bandwidth: Some("upload-bandwidth".into()),
            upload_max_attempts: Some("upload-max-attempts".into()),
            upload_retry_failed_after: Some("upload-retry-failed-after".into()),
            upload_backoff_initial: Some("upload-backoff-initial".into()),
            upload_backoff_max: Some("upload-backoff-max".into()),
            upload_timeout_connect: Some("upload-timeout-connect".into()),
            upload_timeout_request: Some("upload-timeout-request".into()),
        };
        for flag in all.overrides() {
            assert_eq!(format!("--{}", flag.value), flag.flag, "{}", flag.key);
        }
    }

    #[test]
    fn a_child_gets_the_same_settings_as_flags() {
        let flags = SettingFlags {
            config: Some("/etc/kapto.toml".into()),
            bind: None,
            port: Some("7701".into()),
            viewer_stream_port: None,
            tap_socket: None,
            control_socket: None,
            data_dir: Some("/data".into()),
            ..SettingFlags::default()
        };
        assert_eq!(
            flags.to_args(),
            [
                "--config",
                "/etc/kapto.toml",
                "--port",
                "7701",
                "--data-dir",
                "/data"
            ]
        );
    }

    #[test]
    fn every_setting_has_a_flag() {
        let all = SettingFlags {
            config: None,
            bind: Some(String::new()),
            port: Some(String::new()),
            viewer_stream_port: Some(String::new()),
            tap_socket: Some(String::new()),
            control_socket: Some(String::new()),
            data_dir: Some(String::new()),
            packages_dir: Some(String::new()),
            log_level: Some(String::new()),
            viewer_display_rate: Some(String::new()),
            viewer_history: Some(String::new()),
            viewer_history_age: Some(String::new()),
            viewer_stop_grace: Some(String::new()),
            replay_cache_size: Some(String::new()),
            replay_cache_age: Some(String::new()),
            control_timeout: Some(String::new()),
            capture_stop_grace: Some(String::new()),
            disk_warn_below: Some(String::new()),
            disk_stop_below: Some(String::new()),
            preflight_window: Some(String::new()),
            preflight_disk_time: Some(String::new()),
            preflight: Some(String::new()),
            attach_meshes: Some(String::new()),
            capture_compression: Some(String::new()),
            restart_delay: Some(String::new()),
            restart_max_delay: Some(String::new()),
            restart_stable_after: Some(String::new()),
            worker_max_attempts: Some(String::new()),
            worker_pause_while_recording: Some(String::new()),
            upload_when: Some(String::new()),
            upload_pause_while_recording: Some(String::new()),
            upload_delete_local: Some(String::new()),
            upload_min_verified_destinations: Some(String::new()),
            upload_verify: Some(String::new()),
            upload_marker: Some(String::new()),
            upload_part_size: Some(String::new()),
            upload_parallel_parts: Some(String::new()),
            upload_parallel_files: Some(String::new()),
            upload_max_in_flight: Some(String::new()),
            upload_bandwidth: Some(String::new()),
            upload_max_attempts: Some(String::new()),
            upload_retry_failed_after: Some(String::new()),
            upload_backoff_initial: Some(String::new()),
            upload_backoff_max: Some(String::new()),
            upload_timeout_connect: Some(String::new()),
            upload_timeout_request: Some(String::new()),
        };
        let mut covered: Vec<&str> = all.overrides().iter().map(|flag| flag.key).collect();
        let mut settings: Vec<&str> = DEPLOYMENT_KEYS.iter().map(|key| key.path).collect();
        covered.sort_unstable();
        settings.sort_unstable();
        assert_eq!(covered, settings);
    }
}
