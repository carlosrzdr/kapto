// SPDX-License-Identifier: Apache-2.0

//! `kapto config validate` and `kapto config show`.

use std::path::PathBuf;
use std::process::ExitCode;

use kapto_config::{
    DeploymentConfig, Entry, Inputs, Source, default_config_path, read_config_file,
};
use kapto_core::KaptoError;
use kapto_server::settings::{Setting, SettingsOverview};

use crate::SettingFlags;

/// Looks up the real process environment.
fn process_env(name: &str) -> Option<String> {
    std::env::var(name).ok()
}

/// Which configuration file was used, for the first line of the output.
pub(crate) enum FileStatus {
    Read(String),
    MissingDefault(String),
}

/// The configuration folder: the folder of `kapto.toml`, given or not.
/// Profiles and tasks are kept there.
pub(crate) fn config_folder(flags: &SettingFlags) -> PathBuf {
    let path = flags
        .config
        .clone()
        .unwrap_or_else(|| default_config_path(&process_env));
    path.parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .map_or_else(|| PathBuf::from("."), PathBuf::from)
}

/// Reads the configuration file and resolves every setting.
pub(crate) fn load(
    flags: &SettingFlags,
) -> Result<(FileStatus, DeploymentConfig), Vec<KaptoError>> {
    let (path, explicit) = match &flags.config {
        Some(path) => (path.clone(), true),
        None => (default_config_path(&process_env), false),
    };
    let file = read_config_file(&path, explicit).map_err(|error| vec![error])?;
    let status = match &file {
        Some(_) => FileStatus::Read(path.display().to_string()),
        None => FileStatus::MissingDefault(path.display().to_string()),
    };
    let overrides = flags.overrides();
    let config = DeploymentConfig::resolve(&Inputs {
        file: file.as_ref(),
        env: &process_env,
        cli: &overrides,
    })?;
    Ok((status, config))
}

fn describe(status: &FileStatus) -> String {
    match status {
        FileStatus::Read(path) => format!("Configuration file: {path}"),
        FileStatus::MissingDefault(path) => {
            format!("Configuration file: {path} (not found, using defaults)")
        }
    }
}

fn report_errors(errors: &[KaptoError]) -> ExitCode {
    let reports: Vec<String> = errors.iter().map(KaptoError::report).collect();
    eprintln!("{}", reports.join("\n\n"));
    let count = errors.len();
    eprintln!(
        "\n{count} configuration problem{} found.",
        if count == 1 { "" } else { "s" }
    );
    ExitCode::FAILURE
}

/// `kapto config validate`: exit status 0 if the configuration is valid,
/// otherwise 1 with every problem on standard error.
pub(crate) fn validate(flags: &SettingFlags) -> ExitCode {
    let (status, config) = match load(flags) {
        Ok(loaded) => loaded,
        Err(errors) => return report_errors(&errors),
    };
    println!("{}", describe(&status));
    for warning in marker_warnings(&config) {
        eprintln!("warning: {warning}");
    }
    // The profiles and tasks beside it, with their robot packages.
    let (sessions, folder) = match crate::session::load(flags) {
        Ok(loaded) => loaded,
        Err(errors) => return report_errors(&errors),
    };
    println!(
        "Profiles: {}, tasks: {}, in {}",
        sessions.profiles.len(),
        sessions.tasks.len(),
        folder.display()
    );
    let problems: Vec<KaptoError> = sessions
        .profile_problems
        .into_iter()
        .chain(sessions.task_problems)
        .collect();
    if !problems.is_empty() {
        return report_errors(&problems);
    }
    println!("The configuration is valid.");
    ExitCode::SUCCESS
}

/// A warning for each destination that doesn't write the completeness
/// marker: allowed, but whoever reads its bucket then can't tell a whole
/// episode from one still arriving.
fn marker_warnings(config: &DeploymentConfig) -> Vec<String> {
    config
        .upload_destinations
        .iter()
        .filter(|destination| !destination.settings.marker.value)
        .map(|destination| {
            format!(
                "destination `{}` doesn't write the completeness marker `_SUCCESS` ({}): whoever reads {} can't tell a whole episode from one still arriving. Kapto still verifies what it uploads.",
                destination.name, destination.settings.marker.source, destination.url
            )
        })
        .collect()
}

/// Every setting of `config` as the API shows it, read as `status` says.
pub(crate) fn overview(status: &FileStatus, config: &DeploymentConfig) -> SettingsOverview {
    let (file, file_found) = match status {
        FileStatus::Read(path) => (path.clone(), true),
        FileStatus::MissingDefault(path) => (path.clone(), false),
    };
    SettingsOverview {
        file,
        file_found,
        settings: config
            .entries()
            .into_iter()
            .map(|entry| Setting {
                key: entry.key.path.to_owned(),
                default: entry.source == Source::Default,
                source: entry.source.to_string(),
                value: entry.value,
                unit: entry.key.value_type.unit().map(str::to_owned),
                help: entry.key.help.to_owned(),
            })
            .collect(),
    }
}

/// `kapto config show`: every setting, its value, and where it came from.
pub(crate) fn show(flags: &SettingFlags) -> ExitCode {
    match load(flags) {
        Ok((status, config)) => {
            println!("{}\n", describe(&status));
            print!("{}", table(&config.entries()));
            ExitCode::SUCCESS
        }
        Err(errors) => report_errors(&errors),
    }
}

/// Aligned columns: setting, value, source.
fn table(entries: &[Entry]) -> String {
    let rows: Vec<[String; 3]> = entries
        .iter()
        .map(|entry| {
            [
                entry.key.path.to_owned(),
                entry.value.clone(),
                entry.source.to_string(),
            ]
        })
        .collect();
    let header = ["SETTING", "VALUE", "SOURCE"].map(str::to_owned);
    let key_width = rows
        .iter()
        .chain([&header])
        .map(|row| row[0].len())
        .max()
        .unwrap_or(0);
    let value_width = rows
        .iter()
        .chain([&header])
        .map(|row| row[1].len())
        .max()
        .unwrap_or(0);
    std::iter::once(&header)
        .chain(&rows)
        .map(|[key, value, source]| format!("{key:<key_width$}  {value:<value_width$}  {source}\n"))
        .collect::<Vec<_>>()
        .concat()
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use kapto_config::CliOverride;

    use super::*;

    #[test]
    fn the_overview_gives_each_quantity_its_unit() {
        let env = |_: &str| None;
        let config = DeploymentConfig::resolve(&Inputs {
            file: None,
            env: &env,
            cli: &[],
        })
        .unwrap();
        let shown = overview(
            &FileStatus::MissingDefault("/config/kapto.toml".into()),
            &config,
        );
        let unit = |key: &str| {
            shown
                .settings
                .iter()
                .find(|setting| setting.key == key)
                .unwrap()
                .unit
                .clone()
        };
        assert_eq!(unit("viewer.history_age").as_deref(), Some("s"));
        assert_eq!(unit("capture.disk_warn_below").as_deref(), Some("MiB"));
        assert_eq!(unit("viewer.display_rate").as_deref(), Some("Hz"));
        assert_eq!(unit("server.port"), None);
    }

    #[test]
    fn table_aligns_settings_values_and_sources() {
        let env = |name: &str| (name == "HOME").then(|| "/home/u".to_owned());
        let file = kapto_config::ConfigFile {
            path: PathBuf::from("/config/kapto.toml"),
            contents: "[server]\nport = 7701\n".to_owned(),
        };
        let cli = [CliOverride {
            key: "paths.packages",
            value: "/srv/packages".to_owned(),
            flag: "--paths-packages".to_owned(),
        }];
        let config = DeploymentConfig::resolve(&Inputs {
            file: Some(&file),
            env: &env,
            cli: &cli,
        })
        .unwrap();
        assert_eq!(
            config.packages_dir.source,
            Source::Cli {
                flag: "--paths-packages".into()
            }
        );
        assert_eq!(
            table(&config.entries()),
            "\
SETTING                           VALUE                                    SOURCE
server.bind                       127.0.0.1                                default
server.port                       7701                                     /config/kapto.toml:2
viewer.stream_port                9877                                     default
viewer.display_rate               60                                       default
viewer.camera_max_fps             30                                       default
viewer.history                    64                                       default
viewer.history_age                600                                      default
viewer.camera_history             128                                      default
viewer.camera_history_fps         5                                        default
viewer.camera_history_width       640                                      default
viewer.stop_grace                 5                                        default
viewer.replay_cache_size          2048                                     default
viewer.replay_cache_age           7                                        default
viewer.tap_socket                 /home/u/.local/share/kapto/tap.sock      default
viewer.frame_socket               /home/u/.local/share/kapto/frames.sock   default
capture.control_socket            /home/u/.local/share/kapto/capture.sock  default
capture.control_timeout           60                                       default
capture.stop_grace                30                                       default
capture.disk_warn_below           10240                                    default
capture.disk_stop_below           2048                                     default
capture.pre_roll_max              64                                       default
capture.preflight_window          5                                        default
capture.preflight_disk_time       600                                      default
capture.preflight                 refuse                                   default
capture.compression               lz4                                      default
bin.keep_days                     7                                        default
capture.attach_meshes             false                                    default
supervisor.restart_delay          1                                        default
supervisor.restart_max_delay      30                                       default
supervisor.stable_after           60                                       default
worker.max_attempts               3                                        default
worker.pause_while_recording      true                                     default
upload.when                       finalised                                default
upload.pause_while_recording      true                                     default
upload.delete_local               when_disk_low                            default
upload.min_verified_destinations  1                                        default
upload.verify                     server_checksum                          default
upload.marker                     true                                     default
upload.part_size                  16                                       default
upload.parallel_parts             4                                        default
upload.parallel_files             2                                        default
upload.max_in_flight              32                                       default
upload.bandwidth                  0                                        default
upload.max_attempts               8                                        default
upload.retry_failed_after         3600                                     default
upload.backoff_initial            0.5                                      default
upload.backoff_max                300                                      default
upload.timeout_connect            10                                       default
upload.timeout_request            60                                       default
paths.data                        /home/u/.local/share/kapto               default
paths.packages                    /srv/packages                            flag --paths-packages
"
        );
    }
}
