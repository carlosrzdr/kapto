// SPDX-License-Identifier: Apache-2.0

//! `kapto upload`: each destination in `[[upload.destination]]` tested
//! before an episode depends on it (`check`), with the settings and
//! credentials an upload uses, by a probe object written under its prefix,
//! read back and removed; where uploads are (`status`), read from
//! `kapto.db`; and an episode's failed uploads tried again now (`retry`),
//! asked of serve, which alone writes `kapto.db`.

use std::fmt::Write as _;
use std::path::Path;
use std::process::ExitCode;
use std::time::Instant;

use clap::Subcommand;
use kapto_config::UploadDestination;
use kapto_core::{ErrorCode, KaptoError};
use kapto_server::api::{UploadDestination as ApiDestination, UploadsOverview, upload_status};
use kapto_server::jobs::{JOB_SOCKET, JobClient};
use kapto_store::Reader;
use kapto_store::db::DATABASE_FILE;
use kapto_upload::probe;

use crate::SettingFlags;

/// What `kapto upload` does.
#[derive(Subcommand)]
pub(crate) enum UploadCommand {
    /// Test each upload destination, or the one named, as an upload would
    /// use it: write a small probe object under its prefix, read it back
    /// and remove it. Fails if any can't be written or read.
    Check {
        /// The destination's `name`; every destination if left out.
        destination: Option<String>,
    },
    /// Show where uploads are: each destination's episodes uploaded,
    /// uploading and failed, and each upload not done.
    Status,
    /// Try an episode's failed uploads again now, without waiting for
    /// `upload.retry_failed_after`. Needs `kapto serve` running.
    Retry {
        /// The episode's ID.
        episode: String,
    },
}

/// Runs `kapto upload`.
pub(crate) fn upload(flags: &SettingFlags, command: &UploadCommand) -> ExitCode {
    let deployment = match crate::config::load(flags) {
        Ok((_, deployment)) => deployment,
        Err(errors) => {
            for error in errors {
                eprintln!("{}", error.report());
            }
            return ExitCode::FAILURE;
        }
    };
    let data_dir = &deployment.data_dir.value;
    let done = match command {
        UploadCommand::Check { destination } => {
            return check_each(&deployment.upload_destinations, destination.as_ref());
        }
        UploadCommand::Status => status(data_dir, &deployment.upload_destinations),
        UploadCommand::Retry { episode } => retry(data_dir, episode),
    };
    match done {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{}", error.report());
            ExitCode::FAILURE
        }
    }
}

/// Where uploads are, from `kapto.db` in `data_dir`.
fn status(data_dir: &Path, destinations: &[UploadDestination]) -> Result<(), KaptoError> {
    let listed: Vec<ApiDestination> = destinations
        .iter()
        .map(|destination| ApiDestination {
            name: destination.name.clone(),
            url: destination.url.clone(),
        })
        .collect();
    let reader = Reader::open(&data_dir.join(DATABASE_FILE))?;
    print!("{}", described(&upload_status(&reader, &listed)?));
    Ok(())
}

/// `overview` in words, a line for each destination and each upload not
/// done.
fn described(overview: &UploadsOverview) -> String {
    let mut text = String::new();
    if overview.destinations.is_empty() {
        text.push_str("No `[[upload.destination]]` is configured.\n");
    }
    for destination in &overview.destinations {
        let _ = writeln!(
            text,
            "{} ({}): {} uploaded, {} uploading, {} failed{}",
            destination.destination.name,
            destination.destination.url,
            destination.uploaded,
            destination.uploading,
            destination.failed,
            match destination.throttled {
                0 => String::new(),
                throttled => format!(", {throttled} requests throttled"),
            }
        );
    }
    for upload in &overview.uploads {
        let _ = match &upload.failure_code {
            Some(code) => writeln!(
                text,
                "  {} to {}: failed, error[{code}]: {}",
                upload.episode,
                upload.destination,
                upload.failure_message.as_deref().unwrap_or_default()
            ),
            None => writeln!(
                text,
                "  {} to {}: {} of {}",
                upload.episode,
                upload.destination,
                mebibytes(upload.bytes_sent),
                mebibytes(upload.bytes_total)
            ),
        };
    }
    let _ = writeln!(text, "{} episodes waiting to upload", overview.waiting);
    text
}

#[allow(
    clippy::cast_precision_loss,
    reason = "a size shown to a tenth of a MiB"
)]
fn mebibytes(bytes: u64) -> String {
    format!("{:.1} MiB", bytes as f64 / f64::from(1 << 20))
}

/// Asks serve, over the job socket in `data_dir`, to try `episode`'s failed
/// uploads again now.
fn retry(data_dir: &Path, episode: &str) -> Result<(), KaptoError> {
    let mut serve = JobClient::connect(&data_dir.join(JOB_SOCKET))
        .map_err(|error| error.context("needs", "`kapto serve` running".to_owned()))?;
    let destinations = serve.retry_uploads(episode)?;
    println!(
        "{episode}: queued to try again now, to {}",
        destinations.join(", ")
    );
    Ok(())
}

/// Probes each of `destinations`, or the one named `destination`.
fn check_each(destinations: &[UploadDestination], destination: Option<&String>) -> ExitCode {
    let chosen: Vec<&UploadDestination> = destinations
        .iter()
        .filter(|candidate| destination.is_none_or(|name| &candidate.name == name))
        .collect();
    if chosen.is_empty() {
        let error = KaptoError::new(ErrorCode::UploadNoDestination).detail(match destination {
            Some(name) => format!("no `[[upload.destination]]` is named `{name}`"),
            None => "`kapto.toml` has no `[[upload.destination]]`".to_owned(),
        });
        eprintln!("{}", error.report());
        return ExitCode::FAILURE;
    }
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_io()
        .enable_time()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => {
            eprintln!("can't start the network runtime: {error}");
            return ExitCode::FAILURE;
        }
    };
    let mut failed = false;
    for destination in chosen {
        let started = Instant::now();
        match runtime.block_on(probe::check(destination, &|name| std::env::var(name).ok())) {
            Ok(probed) => println!(
                "{}: ok, a probe written, read back and {} in {} ms",
                destination.name,
                if probed.removed {
                    "removed".to_owned()
                } else {
                    format!(
                        "left at `{}`: these credentials may write but not delete, which uploads don't need",
                        probed.key
                    )
                },
                started.elapsed().as_millis()
            ),
            Err(error) => {
                failed = true;
                eprintln!("{}: {}", destination.name, error.report());
            }
        }
    }
    if failed {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}
