// SPDX-License-Identifier: Apache-2.0

//! Kapto configuration: TOML schemas, loading, precedence between sources, and per-episode snapshots.
//!
//! Deployment settings ([`DeploymentConfig`]) are resolved from four layers,
//! each overriding the one before: built-in defaults, `kapto.toml`,
//! environment variables (`KAPTO_<SECTION>_<NAME>`) and command-line flags.
//! Every resolved value records which layer it came from.
//!
//! Stream declarations ([`StreamDeclaration`]) are read from a robot
//! package's `streams.toml` with [`parse_streams`], and which decoded field
//! drives which joint from its `mapping.toml` with [`parse_mapping`].
//!
//! Which quality checks run on its episodes, and their thresholds, are read
//! from its `qa.toml` with [`parse_qa`].
//!
//! Session setups saved in the configuration folder, profiles and tasks,
//! are read with [`parse_profile`] and [`parse_task`].

pub mod carried;
mod deployment;
mod destination;
mod file;
pub mod keys;
mod layout;
mod located;
mod mapping;
mod package;
mod qa;
mod reader;
mod session;
mod source;
mod streams;
mod suggest;
mod upload;
mod value;
mod vocabulary;

pub use deployment::{CliOverride, DeploymentConfig, Entry, Inputs};
pub use destination::{Credentials, DEFAULT_LAYOUT, PLACEHOLDERS, SCHEMES, UploadDestination};
pub use file::{ConfigFile, default_config_path, read_config_file};
pub use keys::{DEPLOYMENT_KEYS, DefaultValue, Key};
pub use layout::{
    Column, LAYOUT_COLUMN_KEYS, LAYOUT_EXAMPLE, LAYOUT_FILE, LAYOUT_PANEL_KEYS, LAYOUTS_FOLDER,
    Layout, MAX_PANELS, MAX_PLOT_FIELDS, Panel, PanelKind, PlotField, default_layout_path,
    parse_layout,
};
pub use mapping::{
    FieldReference, JOINT_ROLES, JointMapping, MappingDeclaration, RoleMapping, joint_role,
    parse_mapping,
};
pub use package::{PACKAGE_FILE, PACKAGE_KEYS, PackageManifest, parse_package_manifest};
pub use qa::{
    AutoencoderAnomaliesSettings, AutoencoderChoice, CameraSettings, DurationSettings,
    FrozenJointsSettings, GripperChatterSettings, IdleSettings, JointJumpsSettings, QA_FILE,
    QA_KEYS, QaSettings, SegmentSettings, StatisticalAnomaliesSettings, StatisticsSelection,
    StreamTimingSettings, StreamsCompleteSettings, StreamsPresentSettings, parse_qa,
};
pub use reader::FileKey;
pub use session::{
    Mode, PROFILE_EXAMPLE, PROFILE_KEYS, PROFILES_DIR, Profile, Sessions, TASK_EXAMPLE, TASK_KEYS,
    TASKS_DIR, Task, load_sessions, parse_profile, parse_task,
};
pub use source::{Effective, Source};
pub use streams::{
    ByteOrder, CAMERA_MESSAGE_TYPES, CameraDeclaration, DEFAULT_MAX_MESSAGE_BYTES,
    DecoderDeclaration, Durability, FramingDeclaration, MAX_DATAGRAM_BYTES, MAX_MESSAGE_BYTES,
    MsgFile, PixelEncoding, QosDeclaration, Reliability, Ros2Declaration, SequenceDeclaration,
    SequenceType, StreamDeclaration, TcpDeclaration, TcpEndpointDeclaration, UdpDeclaration,
    msg_files, parse_streams, parse_streams_with,
};
pub use suggest::closest;
pub use upload::{DeleteLocal, UploadSettings, UploadWhen, Verification};
pub use value::{Compression, PreflightFailures, Value, ValueType};
pub use vocabulary::{
    DEFAULT_KINDS, EVENT_KIND_KEYS, EVENTS_EXAMPLE, EVENTS_FILE, EventKind, KindSource,
    SUBTASK_KINDS, Vocabulary, load_vocabulary, parse_vocabulary,
};
