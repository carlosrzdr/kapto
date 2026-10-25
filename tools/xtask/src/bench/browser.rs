// SPDX-License-Identifier: Apache-2.0

//! `cargo xtask bench browser`: the live view in a browser drawing with the
//! machine's graphics card, watched for a long time at the reference load.
//! `kapto serve` runs with the sample arm driven by the first of 16 streams
//! at 2 kHz; Chromium runs in the Playwright image with the host's
//! `/dev/dri`, and `ui/e2e/live-load.spec.ts` keeps each frame's delay from
//! its newest message's arrival to the browser drawing it, and samples the
//! page's memory every 10 s. The delay must stay within the live view's
//! budget, and the memory flat.

use std::fmt::Write as _;
use std::io::{BufRead, BufReader};
use std::net::TcpListener;
use std::os::unix::fs::MetadataExt;
use std::path::Path;
use std::process::{Child, Command, ExitCode, Stdio};
use std::sync::mpsc;
use std::time::Duration;

use clap::Args;
use serde::{Deserialize, Serialize};

use super::page::{Delays, percentiles};
use super::place::Place;
use super::report::Machine;
use super::{Spec, build, free_ports, work_dir, write_reports, write_streams};

/// The live view watched in a browser at the reference load.
#[derive(Args)]
pub(crate) struct BrowserArgs {
    /// How long the page is watched, in minutes; at least 3, as the first
    /// is left out of the memory's comparison while the page warms up.
    #[arg(long, default_value_t = 10, value_parser = clap::value_parser!(u32).range(3..))]
    minutes: u32,
}

/// The whole delay, a message's arrival to it on screen, at the 95th
/// percentile.
const DELAY_P95_MS: f64 = 150.0;

/// The page's memory may grow this much from its second minute to its
/// last, as a fraction, or `MEMORY_NOISE_MIB`, whichever is more.
const MEMORY_TOLERANCE: f64 = 0.10;
const MEMORY_NOISE_MIB: f64 = 16.0;

/// What the spec writes.
#[derive(Deserialize)]
struct Watched {
    renderer: String,
    delays_ms: Vec<f64>,
    samples: Vec<MemorySample>,
}

#[derive(Clone, Deserialize, Serialize)]
struct MemorySample {
    at_s: f64,
    js_heap_mib: f64,
    browser_mib: f64,
    frames: u64,
}

#[derive(Serialize)]
struct BrowserReport {
    machine: Machine,
    renderer: String,
    minutes: u32,
    frames: usize,
    delay_ms: Option<Delays>,
    /// The average of the second minute and of the last, in MiB.
    browser_mib: (f64, f64),
    js_heap_mib: (f64, f64),
    samples: Vec<MemorySample>,
}

impl BrowserReport {
    fn new(machine: Machine, minutes: u32, watched: Watched) -> Self {
        let end = watched.samples.last().map_or(0.0, |sample| sample.at_s);
        let average = |from: f64, to: f64, field: fn(&MemorySample) -> f64| {
            let window: Vec<f64> = (watched.samples.iter())
                .filter(|sample| sample.at_s > from && sample.at_s <= to)
                .map(field)
                .collect();
            #[allow(clippy::cast_precision_loss, reason = "a handful of samples")]
            let count = window.len().max(1) as f64;
            window.iter().sum::<f64>() / count
        };
        let first_and_last = |field: fn(&MemorySample) -> f64| {
            (average(60.0, 120.0, field), average(end - 60.0, end, field))
        };
        Self {
            machine,
            renderer: watched.renderer,
            minutes,
            frames: watched.delays_ms.len(),
            browser_mib: first_and_last(|sample| sample.browser_mib),
            js_heap_mib: first_and_last(|sample| sample.js_heap_mib),
            delay_ms: percentiles(watched.delays_ms),
            samples: watched.samples,
        }
    }

    fn delay_met(&self) -> bool {
        (self.delay_ms.as_ref()).is_some_and(|delay| delay.p95 < DELAY_P95_MS)
    }

    fn all_met(&self) -> bool {
        self.delay_met() && flat(self.browser_mib) && flat(self.js_heap_mib)
    }

    fn markdown(&self) -> String {
        let machine = &self.machine;
        let yes = |met: bool| if met { "yes" } else { "no" };
        let mut page = format!(
            "# Browser benchmark: the live view watched at full load\n\n\
             Provisional: measured on this machine, not the reference PC. Compare runs \
             only with runs on the same machine.\n\n\
             ## Machine\n\n| | |\n|---|---|\n| CPU | {} |\n| Threads | {} |\n| Memory | {:.1} GiB |\n\
             | Kernel | {} |\n| Drawn with | {} |\n\n\
             16 streams at 2000 Hz, the first driving the sample arm, watched for {} minutes.\n\n",
            machine.cpu,
            machine.threads,
            machine.memory_gib,
            machine.kernel,
            self.renderer,
            self.minutes
        );
        page.push_str("## Delay from a message's arrival to it drawn\n\n");
        let _ = match &self.delay_ms {
            Some(delay) => writeln!(
                page,
                "Over {} frames: p50 {:.1} ms, p95 {:.1} ms, max {:.1} ms; p95 under {DELAY_P95_MS} ms: {}.\n",
                self.frames,
                delay.p50,
                delay.p95,
                delay.max,
                yes(self.delay_met())
            ),
            None => writeln!(page, "The page drew no frames.\n"),
        };
        let _ = write!(
            page,
            "## The page's memory\n\n| | Second minute | Last minute | Flat |\n|---|---|---|---|\n\
             | Browser processes | {:.0} MiB | {:.0} MiB | {} |\n| JavaScript heap | {:.1} MiB | {:.1} MiB | {} |\n\n\
             Flat: at most {:.0} % or {MEMORY_NOISE_MIB} MiB more in the last minute than in the second.\n\n\
             | Minute | Browser processes | JavaScript heap |\n|---|---|---|\n",
            self.browser_mib.0,
            self.browser_mib.1,
            yes(flat(self.browser_mib)),
            self.js_heap_mib.0,
            self.js_heap_mib.1,
            yes(flat(self.js_heap_mib)),
            MEMORY_TOLERANCE * 100.0
        );
        // One sample a minute.
        for sample in self
            .samples
            .iter()
            .filter(|sample| sample.at_s % 60.0 < 10.0)
        {
            let _ = writeln!(
                page,
                "| {:.0} | {:.0} MiB | {:.1} MiB |",
                sample.at_s / 60.0,
                sample.browser_mib,
                sample.js_heap_mib
            );
        }
        page
    }
}

/// Whether memory went from `first` to `last` without growing beyond noise.
fn flat((first, last): (f64, f64)) -> bool {
    last - first <= (first * MEMORY_TOLERANCE).max(MEMORY_NOISE_MIB)
}

/// Runs the browser benchmark and prints its report.
pub(crate) fn run(root: &Path, args: &BrowserArgs) -> ExitCode {
    match measure(root, args) {
        Ok(report) => {
            println!("{}", report.markdown());
            if report.all_met() {
                ExitCode::SUCCESS
            } else {
                eprintln!("Some targets were not met.");
                ExitCode::FAILURE
            }
        }
        Err(problem) => {
            eprintln!("The benchmark couldn't run: {problem}");
            ExitCode::FAILURE
        }
    }
}

fn measure(root: &Path, args: &BrowserArgs) -> Result<BrowserReport, String> {
    let work = work_dir(root, "browser");
    let spec = Spec {
        streams: 16,
        tcp_streams: 0,
        cameras: None,
        rate: 2_000,
        seconds: args.minutes * 60 + 180,
        max_message_bytes: None,
        place: Place::Native,
        tap_socket: None,
        decoded: true,
    };
    let environment = [("KAPTO_WATCH_SECONDS", (args.minutes * 60).to_string())];
    watch(root, &spec, "live-load.spec.ts", &work, &environment, false)?;

    let path = work.join("live-load.json");
    let watched: Watched = std::fs::read_to_string(&path)
        .map_err(|error| format!("can't read {}: {error}", path.display()))
        .and_then(|text| serde_json::from_str(&text).map_err(|error| error.to_string()))?;
    let report = BrowserReport::new(Machine::this(Place::Native), args.minutes, watched);
    write_reports(
        &work,
        &serde_json::to_string_pretty(&report),
        &report.markdown(),
    )?;
    Ok(report)
}

/// Runs `kapto serve` with the sample arm, driven by the first of `spec`'s
/// streams, and the load generator sending them and its cameras, while the
/// Playwright spec `spec_file` watches the page in Chromium drawing with
/// the graphics card, `environment` set for it and `work` its results
/// folder, `/results`. The UI and the release build are made first, the UI
/// unminified if `readable_names`, so a profile of the page names its
/// functions.
pub(super) fn watch(
    root: &Path,
    spec: &Spec,
    spec_file: &str,
    work: &Path,
    environment: &[(&str, String)],
    readable_names: bool,
) -> Result<(), String> {
    let render_node = Path::new("/dev/dri/renderD128");
    let render_group = std::fs::metadata(render_node)
        .map_err(|error| {
            format!(
                "no graphics card to draw with at {}: {error}",
                render_node.display()
            )
        })?
        .gid();
    // The UI first, so the release build embeds it.
    let unminified: &[&str] = if readable_names {
        &["KAPTO_UI_MINIFY=false"]
    } else {
        &[]
    };
    if crate::ui::run_with(root, crate::ui::Task::Build, unminified) != ExitCode::SUCCESS {
        return Err("the UI didn't build".to_owned());
    }
    build(root, Place::Native)?;
    // Kapto's sockets are in its data folder, which must be short.
    let data = std::env::temp_dir().join(format!("kapto-bench-browser-{}", std::process::id()));
    for folder in [work, &data.join("config")] {
        std::fs::create_dir_all(folder)
            .map_err(|error| format!("can't create {}: {error}", folder.display()))?;
    }
    let cameras = spec.cameras.map_or(0, |cameras| cameras.count);
    let port = free_ports(spec.streams + cameras)?;
    let streams = write_streams(work, port, spec)?;
    let mapping = work.join("mapping.toml");
    let config = data.join("config/kapto.toml");
    std::fs::write(&mapping, super::viewer::mapping_of_first_stream())
        .and_then(|()| std::fs::write(&config, ""))
        .map_err(|error| error.to_string())?;
    let binaries = root.join("target/release");

    let (mut kapto, url) = start_serve(&binaries, root, &streams, &mapping, &data, &config)?;
    let mut generators = Vec::new();
    let mut generator = Command::new(binaries.join("kapto-loadgen"));
    generator
        .args(["--streams", &spec.streams.to_string()])
        .args(["--rate", &spec.rate.to_string()])
        .args(["--duration", &spec.seconds.to_string()])
        .args(["--target", &format!("127.0.0.1:{port}")]);
    generators.push(generator);
    if let Some(sent) = spec.cameras {
        let mut generator = Command::new(binaries.join("kapto-loadgen"));
        generator
            .args(["--streams", &sent.count.to_string()])
            .args(["--rate", &sent.fps.to_string()])
            .args(["--duration", &spec.seconds.to_string()])
            .args(["--target", &format!("127.0.0.1:{}", port + spec.streams)])
            .args(["--transport", "tcp-connect"])
            .args(["--camera", &format!("{}x{}", sent.width, sent.height)]);
        generators.push(generator);
    }
    let started: Result<Vec<_>, _> = generators
        .iter_mut()
        .map(|generator| generator.stdout(Stdio::null()).spawn())
        .collect();
    let watched = started
        .map_err(|error| format!("can't start the load generator: {error}"))
        .and_then(|mut generators| {
            let mut extra = vec![
                "--device".to_owned(),
                "/dev/dri".to_owned(),
                "--group-add".to_owned(),
                render_group.to_string(),
                "-e".to_owned(),
                "KAPTO_RESULTS=/results".to_owned(),
                "-v".to_owned(),
                format!("{}:/results", work.display()),
            ];
            for (name, value) in environment {
                extra.extend(["-e".to_owned(), format!("{name}={value}")]);
            }
            let watched = crate::ui::e2e::playwright(root, &url, spec_file, &extra);
            for generator in &mut generators {
                let _ = generator.kill();
                let _ = generator.wait();
            }
            watched
        });
    let _ = Command::new("kill")
        .args(["-INT", &kapto.id().to_string()])
        .status();
    let _ = kapto.wait();
    let _ = std::fs::remove_dir_all(&data);
    watched
}

/// Starts `kapto serve` with the sample arm and the load's streams, and
/// returns once it runs, with its web UI's address.
fn start_serve(
    binaries: &Path,
    root: &Path,
    streams: &Path,
    mapping: &Path,
    data: &Path,
    config: &Path,
) -> Result<(Child, String), String> {
    let (port, stream_port) = two_free_tcp_ports()?;
    let mut child = Command::new(binaries.join("kapto"))
        .arg("serve")
        .arg("--robot")
        .arg(root.join("packages/arm7/urdf/arm7.urdf"))
        .arg("--streams")
        .arg(streams)
        .arg("--mapping")
        .arg(mapping)
        .args(["--port", &port.to_string()])
        .args(["--viewer-stream-port", &stream_port.to_string()])
        .arg("--data-dir")
        .arg(data)
        .arg("--config")
        .arg(config)
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| format!("can't start kapto serve: {error}"))?;
    let stderr = child.stderr.take().ok_or("kapto's output is missing")?;
    let (lines, received) = mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stderr).lines().map_while(Result::ok) {
            eprintln!("  kapto | {line}");
            let _ = lines.send(line);
        }
    });
    loop {
        let line = received
            .recv_timeout(Duration::from_secs(60))
            .map_err(|_| "kapto serve didn't start".to_owned())?;
        if line.starts_with("Kapto is running: ") {
            return Ok((child, format!("http://localhost:{port}")));
        }
    }
}

/// Two loopback TCP ports free now, both held until chosen so they differ.
fn two_free_tcp_ports() -> Result<(u16, u16), String> {
    let bind =
        || TcpListener::bind("127.0.0.1:0").and_then(|held| Ok((held.local_addr()?.port(), held)));
    let ((first, _held), (second, _)) = bind()
        .and_then(|first| Ok((first, bind()?)))
        .map_err(|error| format!("no free port: {error}"))?;
    Ok((first, second))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(at_s: f64, browser_mib: f64) -> MemorySample {
        MemorySample {
            at_s,
            js_heap_mib: 20.0,
            browser_mib,
            frames: 0,
        }
    }

    #[test]
    fn memory_is_compared_between_the_second_minute_and_the_last() {
        let samples = (1..=30)
            .map(|step| f64::from(step) * 10.0)
            // The first minute, warming up, is left out.
            .map(|at| sample(at, if at < 60.0 { 900.0 } else { 300.0 + at / 10.0 }))
            .collect();
        let watched = Watched {
            renderer: "a card".to_owned(),
            delays_ms: vec![20.0, 30.0],
            samples,
        };
        let report = BrowserReport::new(Machine::this(Place::Native), 5, watched);
        // 70 to 120 s, and 250 to 300 s: each minute's six samples.
        assert!((report.browser_mib.0 - 309.5).abs() < 1e-9);
        assert!((report.browser_mib.1 - 327.5).abs() < 1e-9);
        assert!(report.all_met());
    }

    #[test]
    fn memory_is_flat_within_a_tenth_or_some_noise() {
        assert!(flat((300.0, 330.0)));
        assert!(!flat((300.0, 331.0)));
        assert!(flat((20.0, 36.0)), "small amounts within the noise");
        assert!(!flat((20.0, 37.0)));
        assert!(flat((300.0, 250.0)), "shrinking is flat");
    }
}
