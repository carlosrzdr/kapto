// SPDX-License-Identifier: Apache-2.0

//! `cargo xtask profile page`: what the live view costs the page under a
//! scenario's load. `kapto serve` runs with the sample arm and the load;
//! Chromium draws the page with the graphics card in the Playwright image,
//! and `ui/e2e/page-profile.spec.ts` counts the pictures drawn, each frame's
//! delay to the screen, the page's memory and its main thread's busy time,
//! and profiles the main thread. The report compares them with the page's
//! budget in `perf-budget.toml` and lists where the main thread's time goes.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use clap::{Args, ValueEnum};
use serde::{Deserialize, Serialize};

use super::super::browser::watch;
use super::super::page::percentiles;
use super::super::place::Place;
use super::super::{Cameras, Spec, work_dir};

/// How many of the main thread's functions are listed.
const TOP_FUNCTIONS: usize = 15;

/// Arguments of `profile page`.
#[derive(Args, Clone, Copy, Debug)]
pub(crate) struct PageArgs {
    /// The load the page shows; by default the largest, which its budget
    /// is stated for.
    #[arg(long, value_enum, default_value_t = PageScenario::Large)]
    scenario: PageScenario,
    /// How long the page is watched, in minutes; at least 3, as the first
    /// is left out of the memory's comparison while the page warms up.
    #[arg(long, default_value_t = 3, value_parser = clap::value_parser!(u32).range(3..))]
    minutes: u32,
    /// How long the main thread is profiled, in seconds, in the middle.
    #[arg(long, default_value_t = 20)]
    profile_seconds: u32,
}

/// The loads the page is profiled under.
#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub(crate) enum PageScenario {
    /// The reference load: 16 streams at 2 kHz, the first moving the arm.
    Reference,
    /// The reference load with two 1080p cameras at 60 fps beside it.
    Cameras,
    /// 50 streams at 2 kHz and two 1080p cameras at 60 fps: a large robot.
    Large,
    /// The reference load with five 1080p cameras at 60 fps beside it: the
    /// cameras' path under stress.
    FiveCameras,
}

impl PageScenario {
    fn spec(self, seconds: u32) -> Spec {
        let cameras = match self {
            Self::Reference => 0,
            Self::Cameras | Self::Large => 2,
            Self::FiveCameras => 5,
        };
        Spec {
            streams: if self == Self::Large { 50 } else { 16 },
            tcp_streams: 0,
            cameras: (cameras > 0).then_some(Cameras {
                count: cameras,
                fps: 60,
                width: 1920,
                height: 1080,
            }),
            rate: 2_000,
            seconds,
            max_message_bytes: None,
            place: Place::Native,
            tap_socket: None,
            decoded: true,
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::Reference => "reference",
            Self::Cameras => "cameras",
            Self::Large => "large",
            Self::FiveCameras => "five-cameras",
        }
    }
}

/// What the spec writes.
#[derive(Deserialize)]
struct Watched {
    renderer: String,
    delays_ms: Vec<f64>,
    samples: Vec<Sample>,
}

/// Ten seconds of the page.
#[derive(Clone, Deserialize)]
struct Sample {
    at_s: f64,
    browser_mib: f64,
    fps: f64,
    /// Share of the time the main thread ran tasks.
    busy: f64,
    /// Whether the profiler ran meanwhile, slowing the page.
    profiled: bool,
}

/// The file in a page profile's folder `profile report` reads.
pub(super) const MEASURES_FILE: &str = "measures.json";

/// What the page cost, as the budget is held against it.
#[derive(Debug, PartialEq, Serialize, Deserialize)]
pub(super) struct PageMeasures {
    /// Pictures drawn a second, the fewest in 10 s.
    lowest_fps: f64,
    /// A frame's delay to the screen, 95 %; none if no frame came.
    delay_p95_ms: Option<f64>,
    /// The browser's memory, its second minute to its last.
    memory_growth_mib: f64,
    /// The main thread's busy share, the most in 10 s.
    busiest: f64,
}

impl PageMeasures {
    /// From what the page was seen doing, samples the profiler slowed left
    /// out.
    fn new(watched: &Watched) -> Self {
        let unprofiled: Vec<&Sample> = watched
            .samples
            .iter()
            .filter(|sample| !sample.profiled)
            .collect();
        Self {
            lowest_fps: unprofiled
                .iter()
                .map(|sample| sample.fps)
                .fold(f64::INFINITY, f64::min),
            delay_p95_ms: percentiles(watched.delays_ms.clone()).map(|delays| delays.p95),
            memory_growth_mib: memory_growth(&watched.samples),
            busiest: unprofiled
                .iter()
                .map(|sample| sample.busy)
                .fold(0.0, f64::max),
        }
    }

    /// Each measure against `budget`.
    pub(super) fn checks(&self, budget: &PageBudget) -> Vec<PageCheck> {
        let Self {
            lowest_fps,
            delay_p95_ms,
            memory_growth_mib,
            busiest,
        } = *self;
        [
            (
                "Pictures drawn a second, the fewest in 10 s",
                format!("{lowest_fps:.1}"),
                format!("at least {}", budget.min_fps),
                lowest_fps >= budget.min_fps,
            ),
            (
                "A frame's delay to the screen, 95 %",
                delay_p95_ms.map_or_else(|| "no frames".to_owned(), |p95| format!("{p95:.0} ms")),
                format!("at most {} ms", budget.max_delay_p95_ms),
                delay_p95_ms.is_some_and(|p95| p95 <= budget.max_delay_p95_ms),
            ),
            (
                "The browser's memory, second minute to last",
                format!("{memory_growth_mib:+.0} MiB"),
                format!("at most +{} MiB", budget.max_memory_growth_mib),
                memory_growth_mib <= budget.max_memory_growth_mib,
            ),
            (
                "The main thread busy, the most in 10 s",
                format!("{:.0} %", busiest * 100.0),
                format!("at most {:.0} %", budget.max_main_thread_busy * 100.0),
                busiest <= budget.max_main_thread_busy,
            ),
        ]
        .into_iter()
        .map(|(measure, measured, allowed, within)| PageCheck {
            measure: measure.to_owned(),
            measured,
            allowed,
            within,
        })
        .collect()
    }
}

/// One measure of the page against its budget.
#[derive(Debug, PartialEq)]
pub(super) struct PageCheck {
    pub(super) measure: String,
    pub(super) measured: String,
    pub(super) allowed: String,
    pub(super) within: bool,
}

/// What the page may cost, from `perf-budget.toml`'s `[page]`.
#[derive(Debug, PartialEq)]
pub(super) struct PageBudget {
    min_fps: f64,
    max_delay_p95_ms: f64,
    max_memory_growth_mib: f64,
    max_main_thread_busy: f64,
}

/// Reads `[page]`: `min_fps`, `max_delay_p95_ms`, `max_memory_growth_mib`
/// and `max_main_thread_busy`, each required, and a `reason`.
pub(super) fn read_budget(text: &str) -> Result<PageBudget, String> {
    let document = toml_edit::Document::parse(text).map_err(|error| error.to_string())?;
    let page = document
        .get("page")
        .and_then(toml_edit::Item::as_table_like)
        .ok_or("perf-budget.toml has no [page] table")?;
    let number = |key: &str| {
        page.get(key)
            .and_then(|value| {
                value
                    .as_float()
                    .or_else(|| value.as_integer().map(|n| n as f64))
            })
            .filter(|n| *n >= 0.0)
            .ok_or_else(|| format!("page.{key} must be a number, 0 or more"))
    };
    page.get("reason")
        .and_then(toml_edit::Item::as_str)
        .ok_or("page needs a reason")?;
    Ok(PageBudget {
        min_fps: number("min_fps")?,
        max_delay_p95_ms: number("max_delay_p95_ms")?,
        max_memory_growth_mib: number("max_memory_growth_mib")?,
        max_main_thread_busy: number("max_main_thread_busy")?,
    })
}

/// Watches the page under the scenario's load and writes the report; the
/// folder it is in, the report, and whether the page kept to its budget.
pub(super) fn profiled_page(
    root: &Path,
    args: PageArgs,
) -> Result<(PathBuf, String, bool), String> {
    let budget = std::fs::read_to_string(root.join("perf-budget.toml"))
        .map_err(|error| format!("can't read perf-budget.toml: {error}"))
        .and_then(|text| read_budget(&text))?;
    let work = work_dir(root, &format!("page-{}", args.scenario.name()));
    let spec = args.scenario.spec(args.minutes * 60 + 180);
    let environment = [
        ("KAPTO_WATCH_SECONDS", (args.minutes * 60).to_string()),
        ("KAPTO_PROFILE_SECONDS", args.profile_seconds.to_string()),
    ];
    watch(root, &spec, "page-profile.spec.ts", &work, &environment)?;
    let read = |name: &str| {
        std::fs::read_to_string(work.join(name))
            .map_err(|error| format!("can't read {name}: {error}"))
    };
    let watched: Watched =
        serde_json::from_str(&read("page.json")?).map_err(|error| error.to_string())?;
    let profile: Profile =
        serde_json::from_str(&read("page.cpuprofile")?).map_err(|error| error.to_string())?;
    let measures = PageMeasures::new(&watched);
    let (report, checks) = markdown(args, &watched, &profile, &budget);
    let write = |name: &str, text: &str| {
        std::fs::write(work.join(name), text)
            .map_err(|error| format!("can't write {name}: {error}"))
    };
    write("page.md", &report)?;
    write(
        MEASURES_FILE,
        &serde_json::to_string_pretty(&measures).map_err(|error| error.to_string())?,
    )?;
    let within = checks.iter().all(|check| check.within);
    Ok((work, report, within))
}

/// The report and each measure against its budget.
fn markdown(
    args: PageArgs,
    watched: &Watched,
    profile: &Profile,
    budget: &PageBudget,
) -> (String, Vec<PageCheck>) {
    let checks = PageMeasures::new(watched).checks(budget);
    let mut text = format!(
        "# Page profile: the {} load, {} minutes\n\n\
         Drawn by {}. Samples taken while the profiler ran are left out of the checks.\n\n\
         | Measure | Measured | Allowed | Within |\n|---|---|---|---|\n",
        args.scenario.name(),
        args.minutes,
        watched.renderer
    );
    for check in &checks {
        let _ = writeln!(
            text,
            "| {} | {} | {} | {} |",
            check.measure,
            check.measured,
            check.allowed,
            if check.within { "yes" } else { "**no**" }
        );
    }
    text.push_str(&profile.markdown(args.profile_seconds));
    (text, checks)
}

/// How much the browser's memory grew from its second minute to its last,
/// each minute's samples averaged; the first is left out while the page
/// warms up.
fn memory_growth(samples: &[Sample]) -> f64 {
    let end = samples.iter().map(|sample| sample.at_s).fold(0.0, f64::max);
    let mean = |from: f64, to: f64| {
        let within: Vec<f64> = samples
            .iter()
            .filter(|sample| sample.at_s > from && sample.at_s <= to)
            .map(|sample| sample.browser_mib)
            .collect();
        within.iter().sum::<f64>() / within.len().max(1) as f64
    };
    mean(end - 60.0, end) - mean(60.0, 120.0)
}

/// A CPU profile of the main thread, as Chrome's `DevTools` protocol gives it.
#[derive(Deserialize)]
struct Profile {
    nodes: Vec<Node>,
    samples: Vec<u64>,
    #[serde(rename = "timeDeltas")]
    time_deltas: Vec<f64>,
}

#[derive(Deserialize)]
struct Node {
    id: u64,
    #[serde(rename = "callFrame")]
    call_frame: CallFrame,
}

#[derive(Deserialize)]
struct CallFrame {
    #[serde(rename = "functionName")]
    function_name: String,
    url: String,
    #[serde(rename = "lineNumber")]
    line_number: i64,
}

impl Profile {
    /// Each function's own time, in microseconds, the most first: each
    /// sample's time, the gap to the next, given to the function it was in.
    fn functions(&self) -> Vec<(String, f64)> {
        let names: BTreeMap<u64, String> = self
            .nodes
            .iter()
            .map(|node| {
                let frame = &node.call_frame;
                let name = if frame.function_name.is_empty() {
                    "(anonymous)"
                } else {
                    &frame.function_name
                };
                let file = frame.url.rsplit('/').next().unwrap_or("");
                let place = if file.is_empty() {
                    String::new()
                } else {
                    format!(" {file}:{}", frame.line_number + 1)
                };
                (node.id, format!("{name}{place}"))
            })
            .collect();
        let mut own: BTreeMap<&str, f64> = BTreeMap::new();
        for (index, id) in self.samples.iter().enumerate() {
            let time = self.time_deltas.get(index + 1).copied().unwrap_or(0.0);
            if let Some(name) = names.get(id) {
                *own.entry(name).or_default() += time;
            }
        }
        let mut listed: Vec<(String, f64)> = own
            .into_iter()
            .map(|(name, us)| (name.to_owned(), us))
            .collect();
        listed.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
        listed
    }

    fn markdown(&self, seconds: u32) -> String {
        let functions = self.functions();
        let all: f64 = functions.iter().map(|(_, us)| us).sum();
        let share = |us: f64| if all > 0.0 { 100.0 * us / all } else { 0.0 };
        let idle = functions
            .iter()
            .find(|(name, _)| name == "(idle)")
            .map_or(0.0, |(_, us)| *us);
        let mut text = format!(
            "\n## The main thread over {seconds} s profiled\n\n\
             Idle {:.0} % of the time. `page.cpuprofile` opens in Chrome's DevTools, in \
             Performance.\n\n| Function | Own time |\n|---|---:|\n",
            share(idle)
        );
        for (name, us) in functions
            .iter()
            .filter(|(name, _)| name != "(idle)" && name != "(root)")
            .take(TOP_FUNCTIONS)
        {
            let _ = writeln!(text, "| `{name}` | {:.1} % |", share(*us));
        }
        text
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(at_s: f64, browser_mib: f64, fps: f64, profiled: bool) -> Sample {
        Sample {
            at_s,
            browser_mib,
            fps,
            busy: 0.3,
            profiled,
        }
    }

    #[test]
    fn the_budget_is_read_from_the_page_table() {
        let budget = read_budget(
            "[page]\nmin_fps = 50\nmax_delay_p95_ms = 150.0\nmax_memory_growth_mib = 32\n\
             max_main_thread_busy = 0.5\nreason = \"r\"\n",
        )
        .unwrap();
        assert_eq!(budget.min_fps, 50.0);
        assert_eq!(budget.max_main_thread_busy, 0.5);
        assert!(
            read_budget("[page]\nmin_fps = 50\nreason = \"r\"\n")
                .unwrap_err()
                .contains("max_delay_p95_ms")
        );
        let committed =
            std::fs::read_to_string(crate::repo_root().join("perf-budget.toml")).unwrap();
        read_budget(&committed).unwrap();
    }

    #[test]
    fn a_page_is_checked_without_the_samples_the_profiler_slowed() {
        // The ninth, at 90 s, slowed by the profiler.
        let samples: Vec<Sample> = (1..=18)
            .map(|step| {
                let at = f64::from(step) * 10.0;
                let profiled = step == 9;
                sample(
                    at,
                    500.0 + at / 10.0,
                    if profiled { 20.0 } else { 59.0 },
                    profiled,
                )
            })
            .collect();
        let watched = Watched {
            renderer: "a card".to_owned(),
            delays_ms: vec![20.0; 100],
            samples,
        };
        let profile = Profile {
            nodes: vec![],
            samples: vec![],
            time_deltas: vec![],
        };
        let budget = PageBudget {
            min_fps: 50.0,
            max_delay_p95_ms: 150.0,
            max_memory_growth_mib: 32.0,
            max_main_thread_busy: 0.5,
        };
        let args = PageArgs {
            scenario: PageScenario::Large,
            minutes: 3,
            profile_seconds: 10,
        };
        let (report, checks) = markdown(args, &watched, &profile, &budget);
        assert!(checks.iter().all(|check| check.within), "{report}");
        assert!(
            report.contains("| Pictures drawn a second, the fewest in 10 s | 59.0 |"),
            "{report}"
        );
        // 130 to 180 s against 70 to 120 s: 6 MiB more.
        assert!(report.contains("| +6 MiB |"), "{report}");
        let slower = PageBudget {
            min_fps: 60.0,
            ..budget
        };
        let (_, checks) = markdown(args, &watched, &profile, &slower);
        let over: Vec<&str> = checks
            .iter()
            .filter(|check| !check.within)
            .map(|check| check.measure.as_str())
            .collect();
        assert_eq!(over, ["Pictures drawn a second, the fewest in 10 s"]);
    }

    #[test]
    fn each_function_gets_the_time_until_the_next_sample() {
        let node = |id, name: &str, url: &str| Node {
            id,
            call_frame: CallFrame {
                function_name: name.to_owned(),
                url: url.to_owned(),
                line_number: 41,
            },
        };
        let profile = Profile {
            nodes: vec![
                node(1, "(idle)", ""),
                node(2, "drawPlots", "http://localhost/assets/index-abc.js"),
                node(3, "", "http://localhost/assets/index-abc.js"),
            ],
            samples: vec![2, 2, 1, 3],
            time_deltas: vec![0.0, 1000.0, 3000.0, 4000.0],
        };
        assert_eq!(
            profile.functions(),
            [
                ("(idle)".to_owned(), 4000.0),
                ("drawPlots index-abc.js:42".to_owned(), 4000.0),
                ("(anonymous) index-abc.js:42".to_owned(), 0.0),
            ]
        );
        let text = profile.markdown(10);
        assert!(text.contains("Idle 50 % of the time"), "{text}");
        assert!(
            text.contains("| `drawPlots index-abc.js:42` | 50.0 % |"),
            "{text}"
        );
    }
}
