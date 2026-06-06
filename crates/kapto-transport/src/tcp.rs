// SPDX-License-Identifier: Apache-2.0

//! TCP: robots that send a byte stream, connected either way round.
//!
//! One transport is one connection and one stream. Kapto either connects to
//! the robot or listens for the robot to connect; either way, a lost
//! connection is counted and re-established, and recording carries on. The
//! bytes are cut into messages by the stream's [`Framing`].

pub mod framing;

use std::io::{self, Read};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use kapto_capture::{Batch, StreamIndex};
use kapto_core::clock::monotonic_now;
use kapto_core::{ErrorCode, KaptoError, RecvTime};
use socket2::{SockRef, TcpKeepalive};

pub use framing::{Deframer, Framing, LengthOrder};

use crate::backoff::Backoff;
use crate::{Transport, TransportStats};

/// How often a listening transport checks for the robot's connection.
const ACCEPT_POLL: Duration = Duration::from_millis(5);

/// Which side opens the connection.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TcpEndpoint {
    /// Kapto connects to the robot at this address.
    Connect(SocketAddr),
    /// Kapto listens on this local address and the robot connects. Port 0
    /// lets the operating system pick one; [`TcpTransport::local_addr`]
    /// tells which.
    Listen(SocketAddr),
}

/// One TCP connection and the stream its messages belong to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TcpConfig {
    /// The stream every message on this connection belongs to.
    pub stream: StreamIndex,
    /// Which side connects, and where.
    pub endpoint: TcpEndpoint,
    /// How messages are cut out of the bytes.
    pub framing: Framing,
    /// The longest message kept, in bytes; longer ones are skipped and
    /// counted as truncated. At most the batch slot size.
    pub max_message_bytes: usize,
    /// Bytes read from the connection at a time.
    pub read_bytes: usize,
    /// The first wait before connecting again after a failure; each failure
    /// in a row doubles it, up to `reconnect_max`.
    pub reconnect_initial: Duration,
    /// The longest wait between attempts to connect.
    pub reconnect_max: Duration,
    /// How long one attempt to connect may take.
    pub connect_timeout: Duration,
    /// How long a connection may be silent before the operating system
    /// starts checking that the robot is still there, so a robot that lost
    /// power is noticed rather than waited for forever.
    pub keepalive_after: Duration,
}

impl TcpConfig {
    /// A connection for `stream`, with default reading and reconnecting.
    #[must_use]
    pub fn new(
        stream: StreamIndex,
        endpoint: TcpEndpoint,
        framing: Framing,
        max_message_bytes: usize,
    ) -> Self {
        Self {
            stream,
            endpoint,
            framing,
            max_message_bytes,
            read_bytes: 64 * 1024,
            reconnect_initial: Duration::from_millis(100),
            reconnect_max: Duration::from_secs(5),
            connect_timeout: Duration::from_secs(1),
            keepalive_after: Duration::from_secs(5),
        }
    }
}

/// Receives the messages of one TCP connection.
#[derive(Debug)]
pub struct TcpTransport {
    stream: StreamIndex,
    endpoint: TcpEndpoint,
    connect_timeout: Duration,
    keepalive_after: Duration,
    listener: Option<TcpListener>,
    connection: Option<TcpStream>,
    deframer: Deframer,
    too_long_counted: u64,
    backoff: Backoff,
    retry_at: Option<Instant>,
    /// Whether the current connection has delivered any bytes yet; only
    /// then is the backoff reset, so a robot that accepts and at once
    /// hangs up isn't retried in a tight loop.
    delivered: bool,
    /// When the bytes being framed were read.
    read_at: RecvTime,
    stats: Arc<TransportStats>,
}

impl TcpTransport {
    /// Prepares the connection. A listening transport starts listening
    /// here; a connecting one connects on its first receive, and keeps
    /// trying while the robot isn't there.
    ///
    /// ```
    /// use std::io::Write;
    /// use std::net::TcpStream;
    /// use std::time::Duration;
    ///
    /// use kapto_capture::Batch;
    /// use kapto_transport::{Framing, TcpConfig, TcpEndpoint, TcpTransport, Transport};
    ///
    /// let listen = TcpEndpoint::Listen("127.0.0.1:0".parse()?);
    /// let config = TcpConfig::new(0, listen, Framing::Delimiter(b"\n".to_vec()), 1500);
    /// let mut transport = TcpTransport::open(&config)?;
    ///
    /// let mut robot = TcpStream::connect(transport.local_addr().unwrap())?;
    /// robot.write_all(b"joint positions\n")?;
    /// let mut batch = Batch::new(64, 1500);
    /// transport.receive(&mut batch, Duration::from_secs(1))?;
    /// assert_eq!(batch.messages().next(), Some((0, &b"joint positions"[..])));
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    ///
    /// # Errors
    ///
    /// `transport.tcp.listen_failed` if a listening transport can't listen
    /// on its address.
    pub fn open(config: &TcpConfig) -> Result<Self, KaptoError> {
        let listener = match config.endpoint {
            TcpEndpoint::Listen(address) => Some(listen(address)?),
            TcpEndpoint::Connect(_) => None,
        };
        Ok(Self {
            stream: config.stream,
            endpoint: config.endpoint,
            connect_timeout: config.connect_timeout,
            keepalive_after: config.keepalive_after,
            listener,
            connection: None,
            deframer: Deframer::new(
                config.framing.clone(),
                config.max_message_bytes,
                config.read_bytes,
            ),
            too_long_counted: 0,
            backoff: Backoff::new(config.reconnect_initial, config.reconnect_max),
            retry_at: None,
            delivered: false,
            read_at: RecvTime::from_nanos(0),
            stats: Arc::new(TransportStats::new(usize::from(config.stream) + 1)),
        })
    }

    /// The address a listening transport listens on.
    #[must_use]
    pub fn local_addr(&self) -> Option<SocketAddr> {
        self.listener
            .as_ref()
            .and_then(|listener| listener.local_addr().ok())
    }

    /// Whether the robot is connected right now.
    #[must_use]
    pub fn is_connected(&self) -> bool {
        self.connection.is_some()
    }

    /// Moves complete messages already read into `batch`.
    fn take_frames(&mut self, batch: &mut Batch) -> usize {
        let mut added = 0;
        while !batch.is_full() {
            let Some(frame) = self.deframer.next_frame() else {
                break;
            };
            if batch.push(self.stream, frame) {
                added += 1;
            } else {
                self.stats.count_truncated(self.stream);
            }
        }
        let too_long = self.deframer.too_long();
        self.stats
            .count_truncated_by(self.stream, too_long - self.too_long_counted);
        self.too_long_counted = too_long;
        added
    }

    /// Reads what the connection has, waiting up to `wait`. Returns whether
    /// any bytes arrived.
    fn read(&mut self, wait: Duration) -> Result<bool, KaptoError> {
        let Some(connection) = &mut self.connection else {
            return Ok(false);
        };
        // A zero timeout would mean "wait forever"; the smallest the
        // operating system supports is the closest to "don't wait".
        let timeout = wait.max(Duration::from_micros(1));
        let read = connection
            .set_read_timeout(Some(timeout))
            .and_then(|()| connection.read(self.deframer.space()));
        match read {
            Ok(0) => {
                self.disconnected();
                Ok(false)
            }
            Ok(read) => {
                self.read_at = monotonic_now();
                self.deframer.filled(read);
                if !self.delivered {
                    self.delivered = true;
                    self.backoff.reset();
                }
                Ok(true)
            }
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::WouldBlock
                        | io::ErrorKind::TimedOut
                        | io::ErrorKind::Interrupted
                ) =>
            {
                // A robot that reconnects while its old connection still
                // looks open has restarted: the new connection wins.
                self.accept_replacement()?;
                Ok(false)
            }
            Err(_) => {
                self.disconnected();
                Ok(false)
            }
        }
    }

    /// The connection was lost: count it, forget its unread bytes and try
    /// again later.
    fn disconnected(&mut self) {
        self.connection = None;
        self.deframer.reset();
        self.stats.count_disconnect(self.stream);
        if matches!(self.endpoint, TcpEndpoint::Connect(_)) {
            self.retry_at = Some(Instant::now() + self.backoff.next_wait());
        }
    }

    fn connected(&mut self, connection: TcpStream) {
        let keepalive = TcpKeepalive::new().with_time(self.keepalive_after);
        #[cfg(target_os = "linux")]
        let keepalive = keepalive.with_interval(Duration::from_secs(1));
        // Without keepalive a robot that vanished is noticed later, but
        // everything else works, so a failure here isn't one to stop for.
        let _ = SockRef::from(&connection).set_tcp_keepalive(&keepalive);
        self.connection = Some(connection);
        self.deframer.reset();
        self.delivered = false;
        self.retry_at = None;
    }

    /// Waits until `deadline` for the connection to be (re-)established.
    fn connect(&mut self, deadline: Instant) -> Result<(), KaptoError> {
        match self.endpoint {
            TcpEndpoint::Listen(_) => loop {
                if let Some(connection) = self.accept()? {
                    self.connected(connection);
                    return Ok(());
                }
                let now = Instant::now();
                if now >= deadline {
                    return Ok(());
                }
                thread::sleep(ACCEPT_POLL.min(deadline - now));
            },
            TcpEndpoint::Connect(address) => {
                let now = Instant::now();
                if let Some(retry_at) = self.retry_at.filter(|&retry_at| retry_at > now) {
                    thread::sleep(retry_at.min(deadline) - now);
                    if retry_at > deadline {
                        return Ok(());
                    }
                }
                match TcpStream::connect_timeout(&address, self.connect_timeout) {
                    Ok(connection) => self.connected(connection),
                    Err(_) => {
                        // The robot isn't there (yet): keep trying.
                        self.retry_at = Some(Instant::now() + self.backoff.next_wait());
                    }
                }
                Ok(())
            }
        }
    }

    /// A connection waiting to be accepted, if any.
    fn accept(&self) -> Result<Option<TcpStream>, KaptoError> {
        let Some(listener) = &self.listener else {
            return Ok(None);
        };
        match listener.accept() {
            Ok((connection, _)) => {
                connection
                    .set_nonblocking(false)
                    .map_err(|error| self.accept_error(error))?;
                Ok(Some(connection))
            }
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::WouldBlock
                        | io::ErrorKind::Interrupted
                        | io::ErrorKind::ConnectionAborted
                ) =>
            {
                Ok(None)
            }
            Err(error) => Err(self.accept_error(error)),
        }
    }

    fn accept_replacement(&mut self) -> Result<(), KaptoError> {
        if let Some(connection) = self.accept()? {
            self.stats.count_disconnect(self.stream);
            self.connected(connection);
        }
        Ok(())
    }

    fn accept_error(&self, error: io::Error) -> KaptoError {
        KaptoError::new(ErrorCode::TransportTcpAcceptFailed)
            .detail(error.to_string())
            .context(
                "address",
                self.local_addr()
                    .map_or_else(String::new, |a| a.to_string()),
            )
            .source(error)
    }
}

impl Transport for TcpTransport {
    fn receive(&mut self, batch: &mut Batch, timeout: Duration) -> Result<usize, KaptoError> {
        if batch.is_full() {
            return Ok(0);
        }
        let deadline = Instant::now() + timeout;
        let was_empty = batch.is_empty();
        let mut added = self.take_frames(batch);
        while added == 0 {
            let wait = deadline.saturating_duration_since(Instant::now());
            if self.connection.is_none() {
                self.connect(deadline)?;
                if self.connection.is_none() {
                    break;
                }
            } else if self.read(wait)? {
                added = self.take_frames(batch);
            } else if self.connection.is_some() || wait.is_zero() {
                // Nothing arrived in time. A connection lost instead is
                // retried within the time left.
                break;
            }
        }
        if was_empty && added > 0 {
            batch.set_recv_time(self.read_at);
        }
        Ok(added)
    }

    fn stats(&self) -> &Arc<TransportStats> {
        &self.stats
    }
}

fn listen(address: SocketAddr) -> Result<TcpListener, KaptoError> {
    let failed = |error: io::Error| {
        KaptoError::new(ErrorCode::TransportTcpListenFailed)
            .context("address", address.to_string())
            .source(error)
    };
    let listener = TcpListener::bind(address).map_err(failed)?;
    // Accepting must not block: the receiver checks for a connection while
    // reading the current one.
    listener.set_nonblocking(true).map_err(failed)?;
    Ok(listener)
}
