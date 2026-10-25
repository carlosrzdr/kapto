// SPDX-License-Identifier: Apache-2.0

//! Starting Kapto's other processes and keeping them running.
//!
//! Each child gets a thread (`srv-<name>`) that starts it, waits for it to
//! exit and starts it again after a delay that doubles with each quick
//! failure, so a child that can't start doesn't spin. Stopping sends each
//! child SIGTERM, waits up to its grace period, then kills what is left. A
//! supervisor dropped stops its children the same way, so however the
//! server leaves the code holding it, by an error or a panic, no child is
//! left behind.
//!
//! Each child runs in a process group of its own, so Ctrl-C at the
//! terminal reaches only the supervisor, which then stops the children in
//! its own order and says so, rather than seeing them end by themselves
//! and starting them again.
//!
//! Children write to the supervisor's own standard output and error rather
//! than through pipes: if the supervisor dies, a child writing to a pipe
//! nobody reads would fail, and the capture process must keep recording
//! when the supervisor is gone.

use std::ffi::OsString;
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use rustix::process::{Pid, Signal, kill_process};

/// One process to keep running.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChildSpec {
    /// Its name in messages and its thread's name, e.g. `capture`.
    pub name: String,
    /// The program, usually the `kapto` binary itself.
    pub program: PathBuf,
    /// Its arguments.
    pub args: Vec<OsString>,
    /// How long it may take to exit after SIGTERM before it is killed. The
    /// capture process needs long enough to finalise an episode.
    pub stop_grace: Duration,
}

/// How quickly a child that exited is started again.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RestartPolicy {
    /// The wait before the first restart.
    pub initial_delay: Duration,
    /// The longest wait, however often it failed.
    pub max_delay: Duration,
    /// A child that ran at least this long before exiting is restarted
    /// after the initial delay again.
    pub stable_after: Duration,
}

impl Default for RestartPolicy {
    fn default() -> Self {
        Self {
            initial_delay: Duration::from_secs(1),
            max_delay: Duration::from_secs(30),
            stable_after: Duration::from_secs(60),
        }
    }
}

/// What happened to a child.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Event {
    /// It was started.
    Started {
        /// The child's name.
        name: String,
        /// Its process ID.
        pid: u32,
    },
    /// It exited, or couldn't be started, and will be started again.
    Restarting {
        /// The child's name.
        name: String,
        /// How it ended, e.g. `exit status: 1`, or why it didn't start.
        reason: String,
        /// The wait before it is started again.
        delay: Duration,
    },
    /// It ended after being asked to stop.
    Stopped {
        /// The child's name.
        name: String,
        /// Whether it had to be killed because it didn't exit in time.
        killed: bool,
    },
}

/// Something that wants to hear about children: printed by the server,
/// collected by tests.
pub type Listener = Arc<dyn Fn(Event) + Send + Sync>;

/// Running children, each watched by its own thread.
#[derive(Debug)]
pub struct Supervisor {
    stopping: Arc<AtomicBool>,
    children: Vec<Watched>,
}

#[derive(Debug)]
struct Watched {
    spec: ChildSpec,
    /// The running process's ID, or 0 while none runs.
    pid: Arc<AtomicU32>,
    killed: Arc<AtomicBool>,
    thread: JoinHandle<()>,
}

impl Supervisor {
    /// Starts every child and keeps it running until [`Supervisor::stop`].
    ///
    /// # Errors
    ///
    /// A watching thread couldn't be started; the children already started
    /// are stopped again.
    pub fn start(
        specs: Vec<ChildSpec>,
        policy: RestartPolicy,
        listener: &Listener,
    ) -> std::io::Result<Self> {
        let stopping = Arc::new(AtomicBool::new(false));
        let mut supervisor = Self {
            stopping: Arc::clone(&stopping),
            children: Vec::new(),
        };
        for spec in specs {
            let pid = Arc::new(AtomicU32::new(0));
            let killed = Arc::new(AtomicBool::new(false));
            let spawned = thread::Builder::new()
                .name(format!("srv-{}", spec.name))
                .spawn({
                    let (spec, pid, killed) = (spec.clone(), Arc::clone(&pid), Arc::clone(&killed));
                    let (stopping, listener) = (Arc::clone(&stopping), Arc::clone(listener));
                    move || watch(&spec, policy, &pid, &killed, &stopping, &listener)
                });
            match spawned {
                Ok(thread) => supervisor.children.push(Watched {
                    spec,
                    pid,
                    killed,
                    thread,
                }),
                Err(error) => {
                    supervisor.stop();
                    return Err(error);
                }
            }
        }
        Ok(supervisor)
    }

    /// The process ID of the child called `name`, if it is running.
    #[must_use]
    pub fn pid(&self, name: &str) -> Option<u32> {
        self.children
            .iter()
            .find(|child| child.spec.name == name)
            .map(|child| child.pid.load(Ordering::SeqCst))
            .filter(|&pid| pid != 0)
    }

    /// Sends every child SIGTERM, waits for each up to its grace period,
    /// kills those still running, and returns once all have ended. Dropping
    /// the supervisor does the same.
    pub fn stop(self) {
        drop(self);
    }
}

impl Drop for Supervisor {
    fn drop(&mut self) {
        self.stopping.store(true, Ordering::SeqCst);
        let started = Instant::now();
        for child in &self.children {
            signal(&child.pid, Signal::TERM);
        }
        for child in &self.children {
            while !child.thread.is_finished() {
                if started.elapsed() >= child.spec.stop_grace {
                    child.killed.store(true, Ordering::SeqCst);
                    signal(&child.pid, Signal::KILL);
                    break;
                }
                thread::sleep(Duration::from_millis(20));
            }
        }
        for child in self.children.drain(..) {
            let _ = child.thread.join();
        }
    }
}

fn signal(pid: &AtomicU32, signal: Signal) {
    let pid = pid.load(Ordering::SeqCst);
    if let Some(pid) = i32::try_from(pid).ok().and_then(Pid::from_raw) {
        // It may have exited already; nothing to do then.
        let _ = kill_process(pid, signal);
    }
}

/// The watching thread: start, wait, wait a while, start again, until the
/// supervisor stops.
fn watch(
    spec: &ChildSpec,
    policy: RestartPolicy,
    pid: &AtomicU32,
    killed: &AtomicBool,
    stopping: &AtomicBool,
    listener: &Listener,
) {
    let mut delay = policy.initial_delay;
    while !stopping.load(Ordering::SeqCst) {
        let started = Instant::now();
        let reason = match Command::new(&spec.program)
            .args(&spec.args)
            .stdin(Stdio::null())
            .process_group(0)
            .spawn()
        {
            Ok(mut child) => {
                pid.store(child.id(), Ordering::SeqCst);
                // Asked to stop between starting it and noting its ID: it
                // missed the signal, so it gets one now.
                if stopping.load(Ordering::SeqCst) {
                    signal(pid, Signal::TERM);
                }
                listener(Event::Started {
                    name: spec.name.clone(),
                    pid: child.id(),
                });
                let status = child.wait();
                pid.store(0, Ordering::SeqCst);
                match status {
                    Ok(status) => status.to_string(),
                    Err(error) => format!("couldn't wait for it: {error}"),
                }
            }
            Err(error) => format!("couldn't start it: {error}"),
        };
        if stopping.load(Ordering::SeqCst) {
            listener(Event::Stopped {
                name: spec.name.clone(),
                killed: killed.load(Ordering::SeqCst),
            });
            return;
        }
        if started.elapsed() >= policy.stable_after {
            delay = policy.initial_delay;
        }
        listener(Event::Restarting {
            name: spec.name.clone(),
            reason,
            delay,
        });
        let until = Instant::now() + delay;
        while Instant::now() < until && !stopping.load(Ordering::SeqCst) {
            thread::sleep(Duration::from_millis(20));
        }
        delay = (delay * 2).min(policy.max_delay);
    }
    // Stopped while waiting to start it again.
    listener(Event::Stopped {
        name: spec.name.clone(),
        killed: false,
    });
}
