// SPDX-License-Identifier: Apache-2.0

//! The supervisor with small shell scripts as children.

#![cfg(unix)]
#![allow(
    clippy::unwrap_used,
    reason = "test helpers fail the test loudly when the setup itself breaks"
)]

use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use kapto_server::{ChildSpec, Event, Listener, RestartPolicy, Supervisor};
use kapto_testkit::{scratch, wait_until};

fn shell(name: &str, script: &str, stop_grace: Duration) -> ChildSpec {
    ChildSpec {
        name: name.into(),
        program: "sh".into(),
        args: vec!["-c".into(), script.into()],
        stop_grace,
    }
}

const QUICK: RestartPolicy = RestartPolicy {
    initial_delay: Duration::from_millis(50),
    max_delay: Duration::from_millis(400),
    stable_after: Duration::from_secs(60),
};

/// A listener that keeps every event.
fn recorder() -> (Listener, Arc<Mutex<Vec<Event>>>) {
    let events = Arc::new(Mutex::new(Vec::new()));
    let listener: Listener = Arc::new({
        let events = Arc::clone(&events);
        move |event| events.lock().unwrap().push(event)
    });
    (listener, events)
}

#[test]
fn a_child_that_exits_is_started_again_after_a_delay_that_doubles_up_to_the_limit() {
    let (listener, events) = recorder();
    let supervisor = Supervisor::start(
        vec![shell("flaky", "exit 3", Duration::from_secs(1))],
        QUICK,
        &listener,
    )
    .unwrap();
    wait_until("six restarts", || {
        events
            .lock()
            .unwrap()
            .iter()
            .filter(|event| matches!(event, Event::Restarting { .. }))
            .count()
            >= 6
    });
    supervisor.stop();

    let events = events.lock().unwrap();
    let delays: Vec<u128> = events
        .iter()
        .filter_map(|event| match event {
            Event::Restarting { reason, delay, .. } => {
                assert_eq!(reason, "exit status: 3");
                Some(delay.as_millis())
            }
            _ => None,
        })
        .take(6)
        .collect();
    assert_eq!(delays, [50, 100, 200, 400, 400, 400]);
    assert!(matches!(
        events.last(),
        Some(Event::Stopped { killed: false, .. })
    ));
}

#[test]
fn stopping_sends_sigterm_and_waits_for_the_child_to_finish_its_work() {
    let folder = scratch("graceful");
    let marker = folder.join("finished");
    let script = format!(
        "trap 'sleep 0.2; echo done > {}; exit 0' TERM; while :; do sleep 0.05; done",
        marker.display()
    );
    let (listener, events) = recorder();
    let supervisor = Supervisor::start(
        vec![shell("worker", &script, Duration::from_secs(5))],
        QUICK,
        &listener,
    )
    .unwrap();
    wait_until("started", || supervisor.pid("worker").is_some());
    // Let the shell install its trap.
    thread::sleep(Duration::from_millis(200));
    supervisor.stop();

    assert_eq!(std::fs::read_to_string(&marker).unwrap(), "done\n");
    let events = events.lock().unwrap();
    assert!(
        events.contains(&Event::Stopped {
            name: "worker".into(),
            killed: false
        }),
        "{events:?}"
    );
    std::fs::remove_dir_all(folder).unwrap();
}

#[test]
fn a_child_that_ignores_sigterm_is_killed_after_its_grace_period() {
    let (listener, events) = recorder();
    let supervisor = Supervisor::start(
        vec![shell(
            "stubborn",
            "trap '' TERM; while :; do sleep 0.05; done",
            Duration::from_millis(300),
        )],
        QUICK,
        &listener,
    )
    .unwrap();
    wait_until("started", || supervisor.pid("stubborn").is_some());
    thread::sleep(Duration::from_millis(200));
    let stopping = Instant::now();
    supervisor.stop();

    assert!(stopping.elapsed() < Duration::from_secs(3));
    assert!(events.lock().unwrap().contains(&Event::Stopped {
        name: "stubborn".into(),
        killed: true
    }));
}

#[test]
fn a_child_that_ran_long_enough_is_restarted_after_the_initial_delay_again() {
    let (listener, events) = recorder();
    let policy = RestartPolicy {
        stable_after: Duration::from_millis(150),
        ..QUICK
    };
    // Fails fast twice, then runs long enough to count as stable.
    let folder = scratch("stable");
    let count = folder.join("runs");
    let script = format!(
        "echo x >> {0}; [ $(wc -l < {0}) -ge 3 ] && sleep 0.3; exit 1",
        count.display()
    );
    let supervisor = Supervisor::start(
        vec![shell("settling", &script, Duration::from_secs(1))],
        policy,
        &listener,
    )
    .unwrap();
    wait_until("four restarts", || {
        events
            .lock()
            .unwrap()
            .iter()
            .filter(|event| matches!(event, Event::Restarting { .. }))
            .count()
            >= 4
    });
    supervisor.stop();
    let delays: Vec<u128> = events
        .lock()
        .unwrap()
        .iter()
        .filter_map(|event| match event {
            Event::Restarting { delay, .. } => Some(delay.as_millis()),
            _ => None,
        })
        .take(4)
        .collect();
    assert_eq!(delays, [50, 100, 50, 50]);
    std::fs::remove_dir_all(folder).unwrap();
}

#[test]
fn a_program_that_cannot_be_started_is_retried_and_reported() {
    let (listener, events) = recorder();
    let supervisor = Supervisor::start(
        vec![ChildSpec {
            name: "missing".into(),
            program: "/nonexistent/kapto".into(),
            args: Vec::new(),
            stop_grace: Duration::from_secs(1),
        }],
        QUICK,
        &listener,
    )
    .unwrap();
    wait_until("a retry", || {
        events
            .lock()
            .unwrap()
            .iter()
            .any(|event| matches!(event, Event::Restarting { .. }))
    });
    supervisor.stop();
    let events = events.lock().unwrap();
    let Some(Event::Restarting { reason, .. }) = events.first() else {
        panic!("{events:?}");
    };
    assert!(reason.starts_with("couldn't start it:"), "{reason}");
}

#[test]
fn a_supervisor_dropped_without_being_stopped_stops_its_children() {
    let (listener, events) = recorder();
    let supervisor = Supervisor::start(
        vec![shell("sleeper", "exec sleep 30", Duration::from_secs(5))],
        QUICK,
        &listener,
    )
    .unwrap();
    wait_until("started", || supervisor.pid("sleeper").is_some());
    let pid = supervisor.pid("sleeper").unwrap();
    let dropped = Instant::now();
    drop(supervisor);

    assert!(dropped.elapsed() < Duration::from_secs(5), "waited for it");
    assert!(
        !std::path::Path::new(&format!("/proc/{pid}")).exists()
            || !std::process::Command::new("kill")
                .args(["-0", &pid.to_string()])
                .status()
                .unwrap()
                .success(),
        "the child outlived its supervisor"
    );
    assert_eq!(
        events.lock().unwrap().last(),
        Some(&Event::Stopped {
            name: "sleeper".into(),
            killed: false,
        })
    );
}
