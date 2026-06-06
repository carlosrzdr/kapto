// SPDX-License-Identifier: Apache-2.0

//! UDP: unicast or multicast, IPv4 or IPv6, several ports read by one thread.
//!
//! Each socket feeds one stream. A receive waits on every socket at once,
//! then reads each readable socket in batches straight into the batch's free
//! slots (`recvmmsg` on Linux, one `recv` per datagram elsewhere).

mod sys;

use std::io;
use std::net::{IpAddr, Ipv4Addr, SocketAddr, UdpSocket};
use std::sync::Arc;
use std::time::Duration;

use kapto_capture::{Batch, StreamIndex};
use kapto_core::clock::monotonic_now;
use kapto_core::{ErrorCode, KaptoError};
use socket2::{Domain, Protocol, Socket, Type};

use crate::{Transport, TransportStats};

/// One UDP socket and the stream its datagrams belong to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UdpSocketConfig {
    /// The stream every datagram received on this socket belongs to.
    pub stream: StreamIndex,
    /// The local address and port to listen on. Port 0 lets the operating
    /// system pick one; [`UdpTransport::local_addrs`] tells which.
    pub bind: SocketAddr,
    /// A multicast group to join on the default interface. To receive it,
    /// bind to the unspecified address (`0.0.0.0` or `::`) and the group's
    /// port.
    pub multicast_group: Option<IpAddr>,
    /// The only sources allowed to send to this socket; empty allows any.
    /// Datagrams from other addresses are discarded and counted.
    pub allowed_sources: Vec<IpAddr>,
}

impl UdpSocketConfig {
    /// Listens on `bind` for `stream`, accepting any source.
    #[must_use]
    pub fn new(stream: StreamIndex, bind: SocketAddr) -> Self {
        Self {
            stream,
            bind,
            multicast_group: None,
            allowed_sources: Vec::new(),
        }
    }
}

/// Settings shared by every socket of a UDP transport.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct UdpOptions {
    /// Receive buffer to ask the operating system for, per socket, in bytes.
    /// It holds datagrams that arrive while the receiver thread isn't
    /// reading, so it decides how long a pause or burst can be absorbed.
    pub receive_buffer_bytes: usize,
}

impl Default for UdpOptions {
    fn default() -> Self {
        Self {
            receive_buffer_bytes: 8 * 1024 * 1024,
        }
    }
}

/// Receives datagrams from a set of UDP sockets.
#[derive(Debug)]
pub struct UdpTransport {
    sockets: Vec<StreamSocket>,
    readiness: sys::Readiness,
    stats: Arc<TransportStats>,
    warnings: Vec<KaptoError>,
}

#[derive(Debug)]
struct StreamSocket {
    socket: UdpSocket,
    local: SocketAddr,
    stream: StreamIndex,
    allowed: Vec<IpAddr>,
}

impl StreamSocket {
    fn allows(&self, source: Option<IpAddr>) -> bool {
        self.allowed.is_empty()
            || source.is_some_and(|source| self.allowed.contains(&source.to_canonical()))
    }
}

impl UdpTransport {
    /// Opens every socket, joins its multicast group and sets its buffer.
    ///
    /// ```
    /// use std::net::UdpSocket;
    /// use std::time::Duration;
    ///
    /// use kapto_capture::Batch;
    /// use kapto_transport::{Transport, UdpOptions, UdpSocketConfig, UdpTransport};
    ///
    /// let joints = UdpSocketConfig::new(0, "127.0.0.1:0".parse()?);
    /// let mut transport = UdpTransport::open(&[joints], UdpOptions::default())?;
    /// let port = transport.local_addrs().next().unwrap();
    ///
    /// UdpSocket::bind("127.0.0.1:0")?.send_to(b"joint positions", port)?;
    /// let mut batch = Batch::new(64, 1500);
    /// transport.receive(&mut batch, Duration::from_secs(1))?;
    /// assert_eq!(batch.messages().next(), Some((0, &b"joint positions"[..])));
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    ///
    /// # Errors
    ///
    /// `transport.udp.bind_failed` if a socket can't listen on its address,
    /// `transport.udp.multicast_join_failed` if it can't join its group;
    /// `transport.udp.receive_failed` if the sockets can't be waited on.
    pub fn open(sockets: &[UdpSocketConfig], options: UdpOptions) -> Result<Self, KaptoError> {
        let mut warnings = Vec::new();
        let mut opened = Vec::with_capacity(sockets.len());
        for config in sockets {
            let (socket, warning) = open_socket(config, options)?;
            warnings.extend(warning);
            let local = socket
                .local_addr()
                .map_err(|error| bind_error(config, error))?;
            opened.push(StreamSocket {
                socket,
                local,
                stream: config.stream,
                allowed: config
                    .allowed_sources
                    .iter()
                    .map(IpAddr::to_canonical)
                    .collect(),
            });
        }
        let streams = sockets.iter().map(|config| usize::from(config.stream) + 1);
        let readiness = sys::Readiness::new(opened.iter().map(|socket| &socket.socket))
            .map_err(|error| receive_error(error).detail("can't wait on the sockets"))?;
        Ok(Self {
            readiness,
            stats: Arc::new(TransportStats::new(streams.max().unwrap_or(0))),
            sockets: opened,
            warnings,
        })
    }

    /// The address each socket listens on, in configuration order.
    pub fn local_addrs(&self) -> impl Iterator<Item = SocketAddr> + '_ {
        self.sockets.iter().map(|socket| socket.local)
    }

    /// Problems found while opening that don't stop receiving, such as
    /// `transport.udp.receive_buffer_small`.
    #[must_use]
    pub fn warnings(&self) -> &[KaptoError] {
        &self.warnings
    }
}

impl Transport for UdpTransport {
    fn receive(&mut self, batch: &mut Batch, timeout: Duration) -> Result<usize, KaptoError> {
        if batch.is_full() || self.readiness.wait(timeout).map_err(receive_error)? == 0 {
            return Ok(0);
        }
        let now = monotonic_now();
        let was_empty = batch.is_empty();
        let mut added = 0;
        for (socket, readable) in self.sockets.iter().zip(self.readiness.readable()) {
            if readable && !batch.is_full() {
                added += receive_from(socket, batch, &self.stats).map_err(|error| {
                    receive_error(error).context("address", socket.local.to_string())
                })?;
            }
        }
        if was_empty && added > 0 {
            batch.set_recv_time(now);
        }
        Ok(added)
    }

    fn stats(&self) -> &Arc<TransportStats> {
        &self.stats
    }
}

/// Moves every datagram waiting on `socket` that fits into `batch`.
fn receive_from(
    socket: &StreamSocket,
    batch: &mut Batch,
    stats: &TransportStats,
) -> io::Result<usize> {
    let mut datagrams = [sys::Datagram::default(); sys::CHUNK];
    let mut added = 0;
    loop {
        let slot_len = batch.max_message_len();
        let received = sys::receive(&socket.socket, batch.free_slots(), slot_len, &mut datagrams)?;
        let mut kept = 0;
        for (index, datagram) in datagrams[..received].iter().enumerate() {
            if datagram.truncated {
                stats.count_truncated(socket.stream);
            } else if !socket.allows(datagram.source) {
                stats.count_rejected(socket.stream);
            } else {
                batch.commit_from(index - kept, socket.stream, datagram.len);
                kept += 1;
            }
        }
        added += kept;
        if received < sys::CHUNK || batch.is_full() {
            return Ok(added);
        }
    }
}

fn open_socket(
    config: &UdpSocketConfig,
    options: UdpOptions,
) -> Result<(UdpSocket, Option<KaptoError>), KaptoError> {
    let bind_failed = |error| bind_error(config, error);
    let socket = Socket::new(
        Domain::for_address(config.bind),
        Type::DGRAM,
        Some(Protocol::UDP),
    )
    .map_err(bind_failed)?;
    if config.multicast_group.is_some() {
        // Lets other programs on this machine listen to the same group.
        socket.set_reuse_address(true).map_err(bind_failed)?;
    }
    let warning = set_receive_buffer(&socket, options.receive_buffer_bytes, config.bind);
    socket.bind(&config.bind.into()).map_err(bind_failed)?;
    if let Some(group) = config.multicast_group {
        let joined = match group {
            IpAddr::V4(group) => socket.join_multicast_v4(&group, &Ipv4Addr::UNSPECIFIED),
            IpAddr::V6(group) => socket.join_multicast_v6(&group, 0),
        };
        joined.map_err(|error| {
            KaptoError::new(ErrorCode::TransportUdpMulticastJoinFailed)
                .context("group", group.to_string())
                .source(error)
        })?;
    }
    socket.set_nonblocking(true).map_err(bind_failed)?;
    Ok((socket.into(), warning))
}

/// Asks for a receive buffer of `requested` bytes and returns a warning if
/// the operating system gave less, which it does silently.
fn set_receive_buffer(socket: &Socket, requested: usize, bind: SocketAddr) -> Option<KaptoError> {
    let warning = KaptoError::new(ErrorCode::TransportUdpReceiveBufferSmall)
        .context("address", bind.to_string());
    match socket
        .set_recv_buffer_size(requested)
        .and_then(|()| socket.recv_buffer_size())
    {
        Ok(reported) if usable_buffer(reported) >= requested => None,
        Ok(reported) => Some(warning.detail(format!(
            "asked for {requested} bytes, got {}",
            usable_buffer(reported)
        ))),
        Err(error) => Some(warning.source(error)),
    }
}

/// Linux reports twice the size it was asked for, keeping half for its own
/// bookkeeping, so half is what holds datagrams.
fn usable_buffer(reported: usize) -> usize {
    if cfg!(target_os = "linux") {
        reported / 2
    } else {
        reported
    }
}

fn bind_error(config: &UdpSocketConfig, error: io::Error) -> KaptoError {
    KaptoError::new(ErrorCode::TransportUdpBindFailed)
        .context("address", config.bind.to_string())
        .source(error)
}

fn receive_error(error: io::Error) -> KaptoError {
    KaptoError::new(ErrorCode::TransportUdpReceiveFailed)
        .detail(error.to_string())
        .source(error)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_failed_read_is_reported_with_the_operating_systems_reason() {
        let error = receive_error(io::Error::from_raw_os_error(libc::ENETDOWN));
        assert_eq!(error.code(), ErrorCode::TransportUdpReceiveFailed);
        assert!(error.message().contains("down"), "{}", error.message());
    }

    #[test]
    fn allowed_sources_match_ipv4_senders_seen_through_an_ipv6_socket() {
        let socket = StreamSocket {
            socket: UdpSocket::bind("127.0.0.1:0").unwrap(),
            local: "127.0.0.1:0".parse().unwrap(),
            stream: 0,
            allowed: vec!["10.0.0.7".parse().unwrap()],
        };
        assert!(socket.allows(Some("::ffff:10.0.0.7".parse().unwrap())));
        assert!(!socket.allows(Some("10.0.0.8".parse().unwrap())));
        assert!(!socket.allows(None));
    }
}
