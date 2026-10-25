// SPDX-License-Identifier: Apache-2.0

//! The spans of a `profiling` build written as a Chrome trace, for
//! `cargo xtask profile trace` to merge and Perfetto to show: each process
//! started with [`TRACE_DIR_VAR`] set writes `<command>-<pid>.json` there,
//! each span a complete event on its thread from when it is first entered
//! until it closes, so work that waits, as a future polled many times, is
//! one span; timed by the monotonic clock every process shares, so their
//! traces line up.

use std::fs::File;
use std::io::{BufWriter, Write as _};
use std::path::Path;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use kapto_core::clock::monotonic_now;
use serde_json::{Map, Value, json};
use tracing::Subscriber;
use tracing::field::{Field, Visit};
use tracing::span::{Attributes, Id, Record};
use tracing_subscriber::layer::{Context, Layer, SubscriberExt as _};
use tracing_subscriber::registry::LookupSpan;

/// The folder each process writes its trace to; unset, none is written.
pub(crate) const TRACE_DIR_VAR: &str = "KAPTO_PROFILE_TRACE";

/// How often what is written is flushed, besides when the process ends.
const FLUSH_EVERY: Duration = Duration::from_secs(1);

/// Writes this process's trace while it lives, if [`TRACE_DIR_VAR`] says
/// where; the trace is flushed when the guard is dropped.
pub(crate) fn start(command: &str) -> Option<Guard> {
    let folder = std::env::var_os(TRACE_DIR_VAR)?;
    let path = Path::new(&folder).join(format!("{command}-{}.json", std::process::id()));
    let out = match Out::create(&path, command) {
        Ok(out) => Arc::new(Mutex::new(out)),
        Err(error) => {
            eprintln!("can't write the trace {}: {error}", path.display());
            return None;
        }
    };
    let layer = Chrome {
        out: Arc::clone(&out),
    };
    let subscriber = tracing_subscriber::registry().with(layer);
    if tracing::subscriber::set_global_default(subscriber).is_err() {
        return None;
    }
    Some(Guard { out })
}

/// Flushes the trace when dropped.
pub(crate) struct Guard {
    out: Arc<Mutex<Out>>,
}

impl Drop for Guard {
    fn drop(&mut self) {
        let mut out = self.out.lock().unwrap_or_else(PoisonError::into_inner);
        let _ = out.file.flush();
    }
}

/// The trace file: a JSON array of events, left open, as the format
/// allows, so a process that ends abruptly still leaves a trace.
struct Out {
    file: BufWriter<File>,
    /// Threads whose name is written.
    named: Vec<u64>,
    flushed: Instant,
}

impl Out {
    fn create(path: &Path, command: &str) -> std::io::Result<Self> {
        let mut out = Self {
            file: BufWriter::new(File::create(path)?),
            named: Vec::new(),
            flushed: Instant::now(),
        };
        out.file.write_all(b"[\n")?;
        out.event(&json!({
            "ph": "M", "name": "process_name", "pid": std::process::id(),
            "args": { "name": format!("kapto {command}") },
        }))?;
        Ok(out)
    }

    fn event(&mut self, event: &Value) -> std::io::Result<()> {
        serde_json::to_writer(&mut self.file, event)?;
        self.file.write_all(b",\n")?;
        if self.flushed.elapsed() >= FLUSH_EVERY {
            self.file.flush()?;
            self.flushed = Instant::now();
        }
        Ok(())
    }

    /// Writes the span `name` on this thread, from `start_us` until now.
    fn span(&mut self, name: &str, start_us: f64, args: &Map<String, Value>) {
        let tid = u64::from(
            rustix::thread::gettid()
                .as_raw_nonzero()
                .get()
                .unsigned_abs(),
        );
        let pid = std::process::id();
        let mut written = Ok(());
        if !self.named.contains(&tid) {
            self.named.push(tid);
            let thread = std::thread::current();
            written = self.event(&json!({
                "ph": "M", "name": "thread_name", "pid": pid, "tid": tid,
                "args": { "name": thread.name().unwrap_or("unnamed") },
            }));
        }
        let written = written.and_then(|()| {
            self.event(&json!({
                "ph": "X", "name": name, "pid": pid, "tid": tid,
                "ts": start_us, "dur": now_us() - start_us, "args": args,
            }))
        });
        if let Err(error) = written {
            eprintln!("can't write to the trace: {error}");
        }
    }
}

/// Microseconds on the monotonic clock, which every process shares.
fn now_us() -> f64 {
    Duration::from_nanos(monotonic_now().as_nanos()).as_secs_f64() * 1e6
}

/// A span's fields and when it was first entered, kept with it.
#[derive(Default)]
struct Fields {
    args: Map<String, Value>,
    entered_us: Option<f64>,
}

impl Visit for Fields {
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        self.args
            .insert(field.name().to_owned(), Value::String(format!("{value:?}")));
    }

    fn record_str(&mut self, field: &Field, value: &str) {
        self.args
            .insert(field.name().to_owned(), Value::from(value));
    }

    fn record_u64(&mut self, field: &Field, value: u64) {
        self.args
            .insert(field.name().to_owned(), Value::from(value));
    }

    fn record_i64(&mut self, field: &Field, value: i64) {
        self.args
            .insert(field.name().to_owned(), Value::from(value));
    }

    fn record_f64(&mut self, field: &Field, value: f64) {
        self.args
            .insert(field.name().to_owned(), Value::from(value));
    }
}

/// Writes each span, when it closes, as a complete event.
struct Chrome {
    out: Arc<Mutex<Out>>,
}

impl<S> Layer<S> for Chrome
where
    S: Subscriber + for<'a> LookupSpan<'a>,
{
    fn on_new_span(&self, attributes: &Attributes<'_>, id: &Id, context: Context<'_, S>) {
        let mut fields = Fields::default();
        attributes.record(&mut fields);
        if let Some(span) = context.span(id) {
            span.extensions_mut().insert(fields);
        }
    }

    fn on_record(&self, id: &Id, values: &Record<'_>, context: Context<'_, S>) {
        if let Some(span) = context.span(id)
            && let Some(fields) = span.extensions_mut().get_mut::<Fields>()
        {
            values.record(fields);
        }
    }

    fn on_enter(&self, id: &Id, context: Context<'_, S>) {
        if let Some(span) = context.span(id)
            && let Some(fields) = span.extensions_mut().get_mut::<Fields>()
        {
            fields.entered_us.get_or_insert_with(now_us);
        }
    }

    fn on_close(&self, id: Id, context: Context<'_, S>) {
        let Some(span) = context.span(&id) else {
            return;
        };
        let extensions = span.extensions();
        // Never entered: nothing was done in it.
        let Some((entered_us, args)) = extensions
            .get::<Fields>()
            .and_then(|fields| Some((fields.entered_us?, &fields.args)))
        else {
            return;
        };
        let mut out = self.out.lock().unwrap_or_else(PoisonError::into_inner);
        out.span(span.name(), entered_us, args);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_span_is_written_as_a_complete_event_on_its_named_thread() {
        let folder = kapto_testkit::scratch("trace");
        let path = folder.join("record-1.json");
        let out = Arc::new(Mutex::new(Out::create(&path, "record").unwrap()));
        let subscriber = tracing_subscriber::registry().with(Chrome {
            out: Arc::clone(&out),
        });
        std::thread::Builder::new()
            .name("cap-writer".to_owned())
            .spawn(|| {
                tracing::subscriber::with_default(subscriber, || {
                    let _span = kapto_core::span!("write", messages = 64_u64, stream = "arm");
                    std::thread::sleep(Duration::from_millis(2));
                });
            })
            .unwrap()
            .join()
            .unwrap();
        drop(Guard { out });
        // The array is left open; a reader closes it.
        let text = std::fs::read_to_string(&path).unwrap();
        let events: Vec<Value> =
            serde_json::from_str(&format!("{}]", text.trim_end().trim_end_matches(','))).unwrap();
        let named = |kind: &str| {
            events
                .iter()
                .find(|event| event["name"] == kind)
                .unwrap_or_else(|| panic!("no {kind} in {text}"))
        };
        assert_eq!(named("process_name")["args"]["name"], "kapto record");
        assert_eq!(named("thread_name")["args"]["name"], "cap-writer");
        let write = named("write");
        assert_eq!(write["ph"], "X");
        assert_eq!(write["args"]["messages"], 64);
        assert_eq!(write["args"]["stream"], "arm");
        assert!(write["dur"].as_f64().unwrap() >= 2_000.0, "{write}");
        assert_eq!(write["tid"], named("thread_name")["tid"]);
    }

    #[test]
    fn a_future_polled_many_times_is_one_span_from_its_first_poll_to_its_end() {
        let folder = kapto_testkit::scratch("trace-future");
        let path = folder.join("worker-1.json");
        let out = Arc::new(Mutex::new(Out::create(&path, "worker").unwrap()));
        let subscriber = tracing_subscriber::registry().with(Chrome {
            out: Arc::clone(&out),
        });
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .unwrap();
        tracing::subscriber::with_default(subscriber, || {
            let part = async {
                for _ in 0..3 {
                    tokio::time::sleep(Duration::from_millis(1)).await;
                }
            };
            runtime.block_on(kapto_core::instrument!(part, "upload part", index = 3_u64));
        });
        drop(Guard { out });
        let text = std::fs::read_to_string(&path).unwrap();
        let parts: Vec<Value> = serde_json::from_str::<Vec<Value>>(&format!(
            "{}]",
            text.trim_end().trim_end_matches(',')
        ))
        .unwrap()
        .into_iter()
        .filter(|event| event["name"] == "upload part")
        .collect();
        assert_eq!(parts.len(), 1, "{text}");
        assert_eq!(parts[0]["args"]["index"], 3);
        assert!(parts[0]["dur"].as_f64().unwrap() >= 3_000.0, "{text}");
    }
}
