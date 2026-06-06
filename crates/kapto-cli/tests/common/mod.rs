// SPDX-License-Identifier: Apache-2.0

//! What the CLI's tests share: folders and free ports of their own, so
//! tests can run together, HTTP requests to a running API, and a
//! `kapto serve` started for a test.

#![allow(dead_code, reason = "each test file uses only some of the helpers")]

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream, UdpSocket};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

#[cfg(unix)]
pub(crate) mod serve;

/// The repository's root, where `packages/` and `docs/` are.
pub(crate) fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// `N` loopback TCP ports free now, all different. Each is held until all
/// are chosen: a port asked for and let go at once can be handed out again
/// by the next request.
pub(crate) fn free_tcp_ports<const N: usize>() -> [String; N] {
    let held: Vec<TcpListener> = (0..N)
        .map(|_| TcpListener::bind("127.0.0.1:0").unwrap())
        .collect();
    std::array::from_fn(|i| held[i].local_addr().unwrap().port().to_string())
}

/// A loopback UDP port free now, for a robot's stream.
pub(crate) fn free_udp_port() -> String {
    let socket = UdpSocket::bind("127.0.0.1:0").unwrap();
    socket.local_addr().unwrap().port().to_string()
}

/// One HTTP request to the API on `port`: the status code and the body.
/// A body is sent as JSON.
pub(crate) fn http(port: &str, method: &str, path: &str, body: &str) -> (u16, String) {
    let mut stream = TcpStream::connect(format!("127.0.0.1:{port}")).unwrap();
    let headers = if body.is_empty() {
        String::new()
    } else {
        format!(
            "Content-Type: application/json\r\nContent-Length: {}\r\n",
            body.len()
        )
    };
    write!(
        stream,
        "{method} {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n{headers}\r\n{body}"
    )
    .unwrap();
    let mut response = String::new();
    stream.read_to_string(&mut response).unwrap();
    let (head, body) = response.split_once("\r\n\r\n").unwrap();
    let code = head.split(' ').nth(1).unwrap().parse().unwrap();
    (code, body.to_owned())
}

/// Stops a process the way an operator would: SIGTERM, then SIGKILL if it
/// hasn't exited in time. A supervisor stopped this way stops its children
/// too; killed outright, it would leave them running.
#[cfg(unix)]
pub(crate) fn stop_gracefully(mut child: std::process::Child) {
    let _ = std::process::Command::new("kill")
        .args(["-TERM", &child.id().to_string()])
        .status();
    let deadline = Instant::now() + Duration::from_secs(40);
    while Instant::now() < deadline {
        if matches!(child.try_wait(), Ok(Some(_))) {
            return;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    let _ = child.kill();
    let _ = child.wait();
}
