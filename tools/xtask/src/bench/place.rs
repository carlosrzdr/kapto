// SPDX-License-Identifier: Apache-2.0

//! Where `kapto record` runs during a benchmark: directly on this machine, or
//! in the `kapto:dev` container, optionally limited to approximate the
//! reference PC.

use std::path::Path;
use std::process::Command;

use clap::{Args, ValueEnum};

/// The image benchmarked with `--in-docker`, built from the working tree first.
pub(crate) const IMAGE: &str = "kapto:dev";

/// The reference PC: a 4-core desktop or mini-PC with 16 GiB.
const REFERENCE_CPUS: &str = "4";
const REFERENCE_MEMORY: &str = "16g";
const REFERENCE_SHOWN: &str = "4 CPUs and 16 GiB";

/// Where to run `kapto record`.
#[derive(Args, Clone, Copy, Debug)]
pub(crate) struct PlaceArgs {
    /// Run `kapto record` in the kapto:dev container, with host networking,
    /// instead of directly on this machine. The image is rebuilt first.
    #[arg(long)]
    in_docker: bool,
    /// Limit the container to approximate a machine: `reference` is 4 CPUs and
    /// 16 GiB. Implies --in-docker.
    #[arg(long, value_enum, value_name = "MACHINE")]
    limit: Option<LimitTo>,
}

/// Machines a container can be limited to.
#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub(crate) enum LimitTo {
    /// The reference PC: 4 CPUs and 16 GiB.
    Reference,
}

/// Where `kapto record` runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Place {
    /// Directly on this machine.
    Native,
    /// In the container.
    Container {
        /// Limited to the reference PC's CPUs and memory.
        limited: bool,
    },
}

impl From<PlaceArgs> for Place {
    fn from(args: PlaceArgs) -> Self {
        if args.in_docker || args.limit.is_some() {
            Self::Container {
                limited: args.limit == Some(LimitTo::Reference),
            }
        } else {
            Self::Native
        }
    }
}

impl Place {
    /// How the place is described in reports.
    pub(crate) fn describe(self) -> String {
        match self {
            Self::Native => "directly on this machine".to_owned(),
            Self::Container { limited: false } => {
                format!("container {IMAGE}, host networking")
            }
            Self::Container { limited: true } => format!(
                "container {IMAGE}, host networking, limited to {REFERENCE_SHOWN} (the reference PC)"
            ),
        }
    }
}

/// Arguments for `docker` that run `kapto` with `kapto_args` in the container
/// named `name`. The work folder is mounted at the same path, so every path in
/// `kapto_args` means the same inside and out, and the process runs as
/// `uid:gid` so the files it writes belong to the caller.
pub(crate) fn docker_run_args(
    name: &str,
    limited: bool,
    uid: u32,
    gid: u32,
    work: &Path,
    kapto_args: &[String],
) -> Vec<String> {
    let work = work.display().to_string();
    let mut args: Vec<String> = [
        "run",
        "--rm",
        "--name",
        name,
        // UDP from the load generator on this machine's loopback.
        "--network",
        "host",
    ]
    .map(str::to_owned)
    .into();
    args.extend(["--user".to_owned(), format!("{uid}:{gid}")]);
    args.extend(["--volume".to_owned(), format!("{work}:{work}")]);
    if limited {
        args.extend(["--cpus", REFERENCE_CPUS, "--memory", REFERENCE_MEMORY].map(str::to_owned));
    }
    args.push(IMAGE.to_owned());
    args.extend(kapto_args.iter().cloned());
    args
}

/// The host process ID of the main process in container `name`, so it can be
/// measured through `/proc` like a native process.
pub(crate) fn container_pid(name: &str) -> Result<u32, String> {
    let output = Command::new("docker")
        .args(["inspect", "--format", "{{.State.Pid}}", name])
        .output()
        .map_err(|error| format!("can't run docker inspect: {error}"))?;
    String::from_utf8_lossy(&output.stdout)
        .trim()
        .parse()
        .ok()
        .filter(|pid| *pid != 0)
        .ok_or_else(|| format!("container {name} isn't running"))
}

/// Rebuilds the image from the working tree.
pub(crate) fn build_image(root: &Path) -> Result<(), String> {
    let built = Command::new("docker")
        .args(["compose", "build", "--quiet", "kapto"])
        .current_dir(root)
        .status()
        .map_err(|error| format!("can't run docker compose: {error}"))?;
    if built.success() {
        Ok(())
    } else {
        Err("building the kapto:dev image failed".to_owned())
    }
}

/// This process's real user and group IDs, read from `/proc/self/status`.
pub(crate) fn current_ids() -> Result<(u32, u32), String> {
    let status = std::fs::read_to_string("/proc/self/status")
        .map_err(|error| format!("can't read /proc/self/status: {error}"))?;
    ids_from_status(&status).ok_or_else(|| "no Uid or Gid in /proc/self/status".to_owned())
}

/// The first (real) ID on the `Uid:` and `Gid:` lines.
fn ids_from_status(status: &str) -> Option<(u32, u32)> {
    let first = |name: &str| -> Option<u32> {
        let line = status.lines().find(|line| line.starts_with(name))?;
        line[name.len()..].split_whitespace().next()?.parse().ok()
    };
    Some((first("Uid:")?, first("Gid:")?))
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    fn place(in_docker: bool, limit: Option<LimitTo>) -> Place {
        PlaceArgs { in_docker, limit }.into()
    }

    #[test]
    fn a_limit_implies_running_in_the_container() {
        assert_eq!(place(false, None), Place::Native);
        assert_eq!(place(true, None), Place::Container { limited: false });
        assert_eq!(
            place(false, Some(LimitTo::Reference)),
            Place::Container { limited: true }
        );
    }

    #[test]
    fn reports_say_where_kapto_ran() {
        assert_eq!(Place::Native.describe(), "directly on this machine");
        assert_eq!(
            Place::Container { limited: true }.describe(),
            "container kapto:dev, host networking, limited to 4 CPUs and 16 GiB (the reference PC)"
        );
    }

    #[test]
    fn the_container_shares_the_network_and_the_work_folder_and_runs_as_the_caller() {
        let work = PathBuf::from("/repo/target/bench/system-1");
        let kapto_args = ["record", "--duration", "62"].map(str::to_owned);
        let args = docker_run_args("kapto-bench", false, 1000, 1001, &work, &kapto_args);
        assert_eq!(
            args,
            [
                "run",
                "--rm",
                "--name",
                "kapto-bench",
                "--network",
                "host",
                "--user",
                "1000:1001",
                "--volume",
                "/repo/target/bench/system-1:/repo/target/bench/system-1",
                "kapto:dev",
                "record",
                "--duration",
                "62",
            ]
        );
    }

    #[test]
    fn a_limited_container_gets_the_reference_cpus_and_memory() {
        let args = docker_run_args("k", true, 1, 1, Path::new("/w"), &[]);
        let image = args.iter().position(|arg| arg == IMAGE).unwrap();
        assert_eq!(
            args[image - 4..image],
            ["--cpus", "4", "--memory", "16g"].map(str::to_owned)
        );
    }

    #[test]
    fn ids_come_from_the_real_uid_and_gid() {
        let status = "Name:\txtask\nUid:\t1000\t1000\t1000\t1000\nGid:\t1001\t1001\t1001\t1001\n";
        assert_eq!(ids_from_status(status), Some((1000, 1001)));
        assert_eq!(ids_from_status("Name:\txtask\n"), None);
    }

    #[test]
    fn this_process_has_ids() {
        assert!(current_ids().is_ok());
    }
}
