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
mod closing;
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
#[cfg(feature = "profiling")]
mod trace;
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
    /// Address the API and web UI listen on (`server.bind`)
    #[arg(
        long = "server-bind",
        visible_alias = "bind",
        global = true,
        value_name = "ADDRESS"
    )]
    bind: Option<String>,
    /// Port of the API and web UI (`server.port`)
    #[arg(
        long = "server-port",
        visible_alias = "port",
        global = true,
        value_name = "PORT"
    )]
    port: Option<String>,
    /// Port the viewer bridge streams poses and plotted values on (`viewer.stream_port`).
    #[arg(long, global = true, value_name = "PORT")]
    viewer_stream_port: Option<String>,
    /// Unix socket live samples are sent to, for the viewer (`viewer.tap_socket`).
    #[arg(long = "viewer-tap-socket", global = true, value_name = "PATH")]
    tap_socket: Option<String>,
    /// Unix socket the capture process takes its orders on (`capture.control_socket`).
    #[arg(long = "capture-control-socket", global = true, value_name = "PATH")]
    control_socket: Option<String>,
    /// Where recordings and runtime state are stored (`paths.data`)
    #[arg(
        long = "paths-data",
        visible_alias = "data-dir",
        global = true,
        value_name = "DIR"
    )]
    data_dir: Option<String>,
    /// Where robot packages are stored (`paths.packages`)
    #[arg(
        long = "paths-packages",
        visible_alias = "packages-dir",
        global = true,
        value_name = "DIR"
    )]
    packages_dir: Option<String>,
    /// How many times a second the live view moves the robot (`viewer.display_rate`).
    #[arg(long, global = true, value_name = "HZ")]
    viewer_display_rate: Option<String>,
    /// MiB of recent live data the viewer bridge keeps for later viewers (`viewer.history`)
    #[arg(long, global = true, value_name = "MIB")]
    viewer_history: Option<String>,
    /// Seconds the live view can look back over (`viewer.history_age`).
    #[arg(long, global = true, value_name = "SECONDS")]
    viewer_history_age: Option<String>,
    /// Seconds the viewer bridge may take to stop before it is killed (`viewer.stop_grace`).
    #[arg(long, global = true, value_name = "SECONDS")]
    viewer_stop_grace: Option<String>,
    /// MiB replays of recorded episodes may take in the cache (`viewer.replay_cache_size`).
    #[arg(long = "viewer-replay-cache-size", global = true, value_name = "MIB")]
    replay_cache_size: Option<String>,
    /// Days unopened after which a replay leaves the cache (`viewer.replay_cache_age`).
    #[arg(long = "viewer-replay-cache-age", global = true, value_name = "DAYS")]
    replay_cache_age: Option<String>,
    /// Seconds capture keeps recording after serve has gone (`capture.control_timeout`).
    #[arg(
        long = "capture-control-timeout",
        global = true,
        value_name = "SECONDS"
    )]
    control_timeout: Option<String>,
    /// Seconds the capture process may take to stop before it is killed (`capture.stop_grace`).
    #[arg(long, global = true, value_name = "SECONDS")]
    capture_stop_grace: Option<String>,
    /// MiB free below which Kapto warns the disk is running low (`capture.disk_warn_below`).
    #[arg(long = "capture-disk-warn-below", global = true, value_name = "MIB")]
    disk_warn_below: Option<String>,
    /// MiB free below which recording stops and none starts (`capture.disk_stop_below`).
    #[arg(long = "capture-disk-stop-below", global = true, value_name = "MIB")]
    disk_stop_below: Option<String>,
    /// The most MiB set aside for one stream's pre-roll (`capture.pre_roll_max`).
    #[arg(long = "capture-pre-roll-max", global = true, value_name = "MIB")]
    pre_roll_max: Option<String>,
    /// Seconds pre-flight watches the streams (`capture.preflight_window`).
    #[arg(
        long = "capture-preflight-window",
        global = true,
        value_name = "SECONDS"
    )]
    preflight_window: Option<String>,
    /// Seconds of recording pre-flight checks the disk has room for (`capture.preflight_disk_time`).
    #[arg(
        long = "capture-preflight-disk-time",
        global = true,
        value_name = "SECONDS"
    )]
    preflight_disk_time: Option<String>,
    /// What a failed pre-flight check does to a start: refuse or warn (`capture.preflight`).
    #[arg(long = "capture-preflight", global = true, value_name = "POLICY")]
    preflight: Option<String>,
    /// Whether recordings also carry the robot package's meshes (`capture.attach_meshes`).
    #[arg(long = "capture-attach-meshes", global = true, value_name = "BOOL")]
    attach_meshes: Option<String>,
    /// Days an episode deleted by hand stays in the bin (`bin.keep_days`).
    #[arg(long = "bin-keep-days", global = true, value_name = "DAYS")]
    bin_keep_days: Option<String>,
    /// How recordings are compressed: lz4, zstd or none (`capture.compression`).
    #[arg(long, global = true, value_name = "COMPRESSION")]
    capture_compression: Option<String>,
    /// Seconds before a process that exited is started again (`supervisor.restart_delay`).
    #[arg(
        long = "supervisor-restart-delay",
        global = true,
        value_name = "SECONDS"
    )]
    restart_delay: Option<String>,
    /// The longest wait before a process is started again (`supervisor.restart_max_delay`).
    #[arg(
        long = "supervisor-restart-max-delay",
        global = true,
        value_name = "SECONDS"
    )]
    restart_max_delay: Option<String>,
    /// Seconds a process must run for its exit to count as a new failure (`supervisor.stable_after`).
    #[arg(
        long = "supervisor-stable-after",
        global = true,
        value_name = "SECONDS"
    )]
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
    /// Frames a second, at most, of each camera sent to the live view (`viewer.camera_max_fps`).
    #[arg(long = "viewer-camera-max-fps", global = true, value_name = "HZ")]
    camera_max_fps: Option<String>,
    /// Cameras' pictures a second, at most, sent each page, shared among its cameras (`viewer.page_pictures`).
    #[arg(long = "viewer-page-pictures", global = true, value_name = "HZ")]
    page_pictures: Option<String>,
    /// MiB of every camera's pictures the viewer bridge keeps to look back over (`viewer.camera_history`).
    #[arg(long = "viewer-camera-history", global = true, value_name = "MIB")]
    camera_history: Option<String>,
    /// Pictures a second of each camera the viewer bridge keeps to look back over (`viewer.camera_history_fps`).
    #[arg(long = "viewer-camera-history-fps", global = true, value_name = "HZ")]
    camera_history_fps: Option<String>,
    /// Pixels wide, at most, the pictures the viewer bridge keeps to look back over are (`viewer.camera_history_width`).
    #[arg(
        long = "viewer-camera-history-width",
        global = true,
        value_name = "PIXELS"
    )]
    camera_history_width: Option<String>,
    /// Unix socket cameras' frames are sent to, for the viewer (`viewer.frame_socket`).
    #[arg(long = "viewer-frame-socket", global = true, value_name = "PATH")]
    frame_socket: Option<String>,
}

/// The flag that sets each deployment setting, as `(setting, flag)`.
pub const SETTING_FLAGS: &[(&str, &str)] = &[
    ("server.bind", "--server-bind"),
    ("server.port", "--server-port"),
    ("viewer.stream_port", "--viewer-stream-port"),
    ("viewer.tap_socket", "--viewer-tap-socket"),
    ("capture.control_socket", "--capture-control-socket"),
    ("paths.data", "--paths-data"),
    ("paths.packages", "--paths-packages"),
    ("viewer.display_rate", "--viewer-display-rate"),
    ("viewer.history", "--viewer-history"),
    ("viewer.history_age", "--viewer-history-age"),
    ("viewer.stop_grace", "--viewer-stop-grace"),
    ("viewer.replay_cache_size", "--viewer-replay-cache-size"),
    ("viewer.replay_cache_age", "--viewer-replay-cache-age"),
    ("capture.control_timeout", "--capture-control-timeout"),
    ("capture.stop_grace", "--capture-stop-grace"),
    ("capture.disk_warn_below", "--capture-disk-warn-below"),
    ("capture.disk_stop_below", "--capture-disk-stop-below"),
    ("capture.pre_roll_max", "--capture-pre-roll-max"),
    ("capture.preflight_window", "--capture-preflight-window"),
    (
        "capture.preflight_disk_time",
        "--capture-preflight-disk-time",
    ),
    ("capture.preflight", "--capture-preflight"),
    ("capture.attach_meshes", "--capture-attach-meshes"),
    ("bin.keep_days", "--bin-keep-days"),
    ("capture.compression", "--capture-compression"),
    ("supervisor.restart_delay", "--supervisor-restart-delay"),
    (
        "supervisor.restart_max_delay",
        "--supervisor-restart-max-delay",
    ),
    ("supervisor.stable_after", "--supervisor-stable-after"),
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
    ("viewer.camera_max_fps", "--viewer-camera-max-fps"),
    ("viewer.page_pictures", "--viewer-page-pictures"),
    ("viewer.camera_history", "--viewer-camera-history"),
    ("viewer.camera_history_fps", "--viewer-camera-history-fps"),
    (
        "viewer.camera_history_width",
        "--viewer-camera-history-width",
    ),
    ("viewer.frame_socket", "--viewer-frame-socket"),
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
            &self.pre_roll_max,
            &self.preflight_window,
            &self.preflight_disk_time,
            &self.preflight,
            &self.attach_meshes,
            &self.bin_keep_days,
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
            &self.camera_max_fps,
            &self.page_pictures,
            &self.camera_history,
            &self.camera_history_fps,
            &self.camera_history_width,
            &self.frame_socket,
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
    /// Record one episode from the streams in a streams.toml, for a set time.
    ///
    /// Each stream may come over UDP, TCP or ROS 2, as the streams.toml
    /// declares it; the episode is written into the data directory. Exits
    /// with 2 if no stream received anything (the episode is kept all the
    /// same), and with 1 if recording failed.
    Record {
        /// The streams.toml declaring each stream's input (a UDP port, a TCP
        /// address or a ROS 2 topic) and rate.
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
    /// Receive the streams and record episodes on command; serve starts it.
    ///
    /// Receives every stream declared in a streams.toml for as long as it
    /// runs, feeding the live view, and records episodes when told to over
    /// the control socket; SIGINT or SIGTERM finalises the episode being
    /// recorded and stops.
    #[command(hide = true)]
    Capture {
        /// The streams.toml declaring each stream's input and rate.
        #[arg(long, value_name = "FILE")]
        streams: PathBuf,
        /// The mapping.toml saying which decoded value drives which joint,
        /// carried by each recording when the streams aren't declared in a
        /// robot package, so its quality checks know the joints.
        #[arg(long, value_name = "FILE")]
        mapping: Option<PathBuf>,
        /// End once process PID has ended and no episode is recorded, an
        /// episode being finalised first: serve passes its own, so this
        /// ends with it.
        #[arg(long, value_name = "PID")]
        parent: Option<u32>,
    },
    /// Start, stop, mark or delete the episodes of a running `kapto serve`.
    ///
    /// Talks to the capture process `kapto serve` started (or a `kapto
    /// capture` of its own): starts and stops episodes, marks events in
    /// them, runs its pre-flight checks or shows what it is doing; and puts
    /// episodes in the bin or back from it.
    #[command(subcommand)]
    Episode(episode::EpisodeCommand),
    /// List, show, check or import robot packages.
    ///
    /// Lists the robot packages Kapto finds, shows one (what identifies it
    /// and its files), checks one for problems, or makes one from a robot
    /// description found elsewhere.
    #[command(subcommand)]
    Package(package::PackageCommand),
    /// List the session profiles in the configuration folder, or show one.
    ///
    /// A profile says with which robot, by whom, where and how episodes are
    /// recorded.
    #[command(subcommand)]
    Profile(session::ProfileCommand),
    /// List the tasks in the configuration folder, or show one.
    ///
    /// A task says what the robot is asked to do.
    #[command(subcommand)]
    Task(session::TaskCommand),
    /// Show a recording's or a packet capture's messages as named values.
    ///
    /// A recording is decoded with the layouts stored in it; streams without
    /// one are shown as raw bytes. With --pcap and --package, a packet
    /// capture is decoded with a package's streams.
    Decode(decode::DecodeArgs),
    /// Try Kapto without a robot: a sample arm and a synthetic robot.
    ///
    /// Runs Kapto with the built-in sample arm and a synthetic robot sending
    /// its joint states; open the address it prints.
    Demo(demo::DemoArgs),
    /// Show Kapto to people: the demo, narrated step by step.
    ///
    /// Records ordinary episodes and then one in which the robot misbehaves,
    /// and says what Kapto found and where to watch it.
    Showcase(showcase::ShowcaseArgs),
    /// Run Kapto: recording, the live view, the worker, the API and web UI.
    ///
    /// Starts the capture process, the viewer bridge and the worker, keeps
    /// them running, serves the API and web UI, and stops everything on
    /// SIGINT or SIGTERM.
    Serve(serve::ServeArgs),
    /// Stream the robot moving live to the web UI; serve starts it.
    ///
    /// Streams the robot moving to the web UI's live view, from the samples
    /// a running capture process or recording sends; runs until
    /// interrupted.
    #[command(hide = true)]
    VizBridge(viz_bridge::VizBridgeArgs),
    /// Do the jobs `kapto serve` queues; serve starts it.
    ///
    /// Jobs such as checking each finalised episode's files against its
    /// manifest, one at a time; runs until SIGINT or SIGTERM.
    #[command(hide = true)]
    Worker(worker::WorkerArgs),
    /// List the ROS 2 topics on this machine's network, with their types.
    ///
    /// For declaring a ROS 2 robot's streams. Only the Kapto of the
    /// `kapto-ros2` image is built with ROS 2.
    Topics(topics::TopicsArgs),
    /// Write an episode in another tool's format, such as a rosbag2 bag.
    ///
    /// An episode's ROS 2 topics as a rosbag2 bag that `ros2 bag play` plays
    /// back on the robot's topics.
    #[command(subcommand)]
    Export(export::ExportCommand),
    /// Test the upload destinations, see where uploads are, or retry them.
    ///
    /// Tests that each destination in `[[upload.destination]]` can be written
    /// to as an upload would, shows where uploads are, or tries an episode's
    /// failed uploads again.
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

/// `count` and the word for it: `one` for a single thing, otherwise `many`.
pub(crate) fn counted(count: usize, one: &str, many: &str) -> String {
    format!("{count} {}", if count == 1 { one } else { many })
}

/// The setting flags every help shows; the others only in `--help`.
const SHOWN_SETTINGS: &[&str] = &["config", "bind", "port", "data_dir", "packages_dir"];

/// The heading the setting flags are listed under, after each command's own.
const SETTINGS_HEADING: &str = "Settings";

/// The full command-line definition: commands, flags and help texts.
#[must_use]
pub fn command() -> clap::Command {
    // Every setting has a flag, but most are set once in kapto.toml: listed
    // in every `-h`, they would bury each command's own options.
    with_footers(Cli::command().mut_args(|arg| {
        let shown = SHOWN_SETTINGS.contains(&arg.get_id().as_str());
        arg.hide_short_help(!shown).help_heading(SETTINGS_HEADING)
    }))
}

const SHORT_HELP_FOOTER: &str =
    "More settings: --help lists a flag for each; `kapto config show` shows their values.";

const LONG_HELP_FOOTER: &str = "Each setting can also be set in kapto.toml or a KAPTO_<SECTION>_<NAME> \
     environment variable; `kapto config show` shows their values and \
     docs/reference/configuration.md describes them.";

/// `command` and each of its subcommands, with where to find the settings.
fn with_footers(command: clap::Command) -> clap::Command {
    command
        .after_help(SHORT_HELP_FOOTER)
        .after_long_help(LONG_HELP_FOOTER)
        .mut_subcommands(with_footers)
}

/// Parses the process arguments and runs the chosen command.
#[cfg(feature = "run")]
#[must_use]
pub fn run() -> ExitCode {
    use clap::FromArgMatches as _;
    let matches = command().get_matches();
    let cli = Cli::from_arg_matches(&matches).unwrap_or_else(|error| error.exit());
    // Each process's spans, named for its command, in a profiling build.
    #[cfg(feature = "profiling")]
    let _trace = trace::start(matches.subcommand_name().unwrap_or("kapto"));
    match cli.command {
        Command::Config(ConfigCommand::Validate) => config::validate(&cli.settings),
        Command::Config(ConfigCommand::Show) => config::show(&cli.settings),
        Command::Decode(args) => decode::decode(&cli.settings, &args),
        Command::Capture {
            streams,
            mapping,
            parent,
        } => capture::capture(&cli.settings, &streams, mapping.as_deref(), parent),
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

    /// The help `kapto <subcommand> -h` (or `--help`, if `long`) prints.
    fn help_of(subcommand: &str, long: bool) -> String {
        let mut root = command();
        root.build();
        let Some(sub) = root.find_subcommand_mut(subcommand) else {
            panic!("no subcommand {subcommand}");
        };
        let help = if long {
            sub.render_long_help()
        } else {
            sub.render_help()
        };
        help.to_string()
    }

    #[test]
    fn short_help_lists_the_command_and_a_few_settings_but_not_every_setting() {
        let help = help_of("record", false);
        for shown in [
            "--streams",
            "--duration",
            "--config",
            "--data-dir",
            "--port",
        ] {
            assert!(help.contains(shown), "{shown} missing from:\n{help}");
        }
        assert!(!help.contains("--upload-part-size"), "{help}");
        let own = help.find("--streams");
        assert!(own < help.find(SETTINGS_HEADING), "{help}");
        assert!(
            help.find(SETTINGS_HEADING) < help.find("--config"),
            "{help}"
        );
        assert!(help.contains("--help lists a flag for each"), "{help}");
    }

    #[test]
    fn long_help_lists_every_setting_under_its_own_heading() {
        let help = help_of("record", true);
        let heading = help.find(SETTINGS_HEADING);
        let flag = help.find("--upload-part-size");
        assert!(heading.is_some() && flag > heading, "{help}");
        for (_, flag) in SETTING_FLAGS {
            assert!(help.contains(flag), "{flag} missing from:\n{help}");
        }
        assert!(help.contains("docs/reference/configuration.md"), "{help}");
    }

    #[test]
    fn the_processes_serve_starts_are_left_out_of_the_command_list() {
        let help = command().render_help().to_string();
        for hidden in ["capture", "viz-bridge", "worker"] {
            assert!(
                !help
                    .lines()
                    .any(|line| line.trim_start().starts_with(hidden)),
                "{hidden} listed in:\n{help}"
            );
        }
        assert!(
            help.lines()
                .any(|line| line.trim_start().starts_with("serve"))
        );
    }

    #[test]
    fn each_command_is_summed_up_in_one_short_line() {
        for sub in command().get_subcommands() {
            let about = sub.get_about().map(ToString::to_string).unwrap_or_default();
            assert!(
                !about.is_empty() && about.len() <= 72,
                "{}: {about:?} ({} chars)",
                sub.get_name(),
                about.len()
            );
        }
    }

    #[test]
    fn a_count_of_one_takes_the_singular() {
        assert_eq!(counted(1, "camera", "cameras"), "1 camera");
        assert_eq!(counted(0, "camera", "cameras"), "0 cameras");
        assert_eq!(counted(16, "stream", "streams"), "16 streams");
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
            bind: Some("server-bind".into()),
            port: Some("server-port".into()),
            viewer_stream_port: Some("viewer-stream-port".into()),
            tap_socket: Some("viewer-tap-socket".into()),
            control_socket: Some("capture-control-socket".into()),
            data_dir: Some("paths-data".into()),
            packages_dir: Some("paths-packages".into()),
            viewer_display_rate: Some("viewer-display-rate".into()),
            viewer_history: Some("viewer-history".into()),
            viewer_history_age: Some("viewer-history-age".into()),
            viewer_stop_grace: Some("viewer-stop-grace".into()),
            replay_cache_size: Some("viewer-replay-cache-size".into()),
            replay_cache_age: Some("viewer-replay-cache-age".into()),
            control_timeout: Some("capture-control-timeout".into()),
            capture_stop_grace: Some("capture-stop-grace".into()),
            disk_warn_below: Some("capture-disk-warn-below".into()),
            disk_stop_below: Some("capture-disk-stop-below".into()),
            pre_roll_max: Some("capture-pre-roll-max".into()),
            preflight_window: Some("capture-preflight-window".into()),
            preflight_disk_time: Some("capture-preflight-disk-time".into()),
            preflight: Some("capture-preflight".into()),
            attach_meshes: Some("capture-attach-meshes".into()),
            bin_keep_days: Some("bin-keep-days".into()),
            capture_compression: Some("capture-compression".into()),
            restart_delay: Some("supervisor-restart-delay".into()),
            restart_max_delay: Some("supervisor-restart-max-delay".into()),
            restart_stable_after: Some("supervisor-stable-after".into()),
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
            camera_max_fps: Some("viewer-camera-max-fps".into()),
            page_pictures: Some("viewer-page-pictures".into()),
            camera_history: Some("viewer-camera-history".into()),
            camera_history_fps: Some("viewer-camera-history-fps".into()),
            camera_history_width: Some("viewer-camera-history-width".into()),
            frame_socket: Some("viewer-frame-socket".into()),
        };
        for flag in all.overrides() {
            assert_eq!(format!("--{}", flag.value), flag.flag, "{}", flag.key);
            // One rule for every flag: its setting's section and name.
            let ruled = format!("--{}", flag.key.replace(['.', '_'], "-"));
            assert_eq!(flag.flag, ruled, "{}", flag.key);
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
                "--server-port",
                "7701",
                "--paths-data",
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
            pre_roll_max: Some(String::new()),
            preflight_window: Some(String::new()),
            preflight_disk_time: Some(String::new()),
            preflight: Some(String::new()),
            attach_meshes: Some(String::new()),
            bin_keep_days: Some(String::new()),
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
            camera_max_fps: Some(String::new()),
            page_pictures: Some(String::new()),
            camera_history: Some(String::new()),
            camera_history_fps: Some(String::new()),
            camera_history_width: Some(String::new()),
            frame_socket: Some(String::new()),
        };
        let mut covered: Vec<&str> = all.overrides().iter().map(|flag| flag.key).collect();
        let mut settings: Vec<&str> = DEPLOYMENT_KEYS.iter().map(|key| key.path).collect();
        covered.sort_unstable();
        settings.sort_unstable();
        assert_eq!(covered, settings);
    }
}
