// SPDX-License-Identifier: Apache-2.0

//! The settings `kapto serve` runs with, each with where its value came
//! from, as `kapto config show` prints them. Worked out once when serve
//! starts: a changed setting takes effect when serve is started again.

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// Every setting serve runs with.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct SettingsOverview {
    /// The configuration file serve read, or would have read.
    pub file: String,
    /// Whether that file was there. Without it, every setting not given
    /// otherwise has its default.
    pub file_found: bool,
    /// Every setting, in the order the configuration reference lists them.
    pub settings: Vec<Setting>,
}

/// One setting.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct Setting {
    /// Its name, e.g. `server.port`.
    pub key: String,
    /// Its value, as it would be written in `kapto.toml`.
    pub value: String,
    /// Where the value came from: `default`, a file and line such as
    /// `/config/kapto.toml:2`, an environment variable such as
    /// `env KAPTO_SERVER_PORT`, or a flag such as `flag --port`.
    pub source: String,
    /// Whether the value is the built-in default.
    pub default: bool,
    /// What it does.
    pub help: String,
}
