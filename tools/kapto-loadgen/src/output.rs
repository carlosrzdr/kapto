// SPDX-License-Identifier: Apache-2.0

//! Where messages go: UDP datagrams, frames on TCP connections, or ROS 2
//! messages on topics ([`crate::ros2`]).
//!
//! Over TCP each message is one frame: its length as 4 bytes little-endian,
//! then the message. In Kapto's `streams.toml`:
//!
//! ```toml
//! [streams.joints_0.tcp]
//! listen = "0.0.0.0:9000"     # or connect = "…" with --transport tcp-listen
//! framing = { kind = "length_prefix", length_bytes = 4, byte_order = "little" }
//! ```
//!
//! A stream without a connection doesn't send: its messages are counted as
//! send errors, as a robot would drop them. Listening, it takes a waiting
//! connection at each message; connecting, it tries again at most once a
//! second, so a missing receiver doesn't hold up the other streams.

use std::io::{self, Write};
use std::net::{SocketAddr, TcpListener, TcpStream, UdpSocket};
use std::time::{Duration, Instant};

use crate::Config;
use crate::payload::{self, Sample};

/// How the streams are sent.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Transport {
    /// UDP datagrams to each stream's port.
    #[default]
    Udp,
    /// TCP: the generator connects to each stream's port, where Kapto
    /// listens.
    TcpConnect,
    /// TCP: the generator listens on each stream's port and Kapto connects.
    TcpListen,
    /// ROS 2: each stream is a topic of `sensor_msgs/msg/JointState`
    /// messages ([`Config::stream_topic`]); only with the `ros2` feature.
    Ros2,
}

impl Transport {
    /// The name used on the command line.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Udp => "udp",
            Self::TcpConnect => "tcp-connect",
            Self::TcpListen => "tcp-listen",
            Self::Ros2 => "ros2",
        }
    }
}

/// The bytes before each message on TCP: its length.
pub const FRAME_HEADER_BYTES: usize = 4;

/// How long a connection attempt may hold up one tick.
const CONNECT_TIMEOUT: Duration = Duration::from_millis(50);

/// How long a stream without a connection waits before trying again.
const RETRY_EVERY: Duration = Duration::from_secs(1);

/// The open sockets of every stream.
pub(crate) enum Output {
    Udp {
        socket: UdpSocket,
        targets: Vec<SocketAddr>,
        message: [u8; payload::SIZE],
    },
    Tcp {
        links: Vec<Link>,
        frame: Vec<u8>,
    },
    #[cfg(feature = "ros2")]
    Ros2(Box<crate::ros2::Publishers>),
}

/// One stream's TCP connection, or what it takes to get one.
pub(crate) struct Link {
    target: SocketAddr,
    listener: Option<TcpListener>,
    stream: Option<TcpStream>,
    next_attempt: Instant,
}

impl Output {
    /// Opens what `config`'s transport needs to send each stream; for
    /// [`Transport::TcpListen`] that includes listening on each.
    pub(crate) fn open(config: &Config) -> io::Result<Self> {
        let targets: Vec<SocketAddr> = (0..config.streams)
            .map(|stream| config.stream_target(stream))
            .collect();
        let transport = config.transport;
        match transport {
            Transport::Udp => {
                let any = match targets.first() {
                    Some(target) if target.is_ipv6() => "[::]:0",
                    _ => "0.0.0.0:0",
                };
                Ok(Self::Udp {
                    socket: UdpSocket::bind(any)?,
                    targets,
                    message: [0; payload::SIZE],
                })
            }
            Transport::TcpConnect | Transport::TcpListen => {
                let now = Instant::now();
                let links = targets
                    .into_iter()
                    .map(|target| {
                        let listener = if transport == Transport::TcpListen {
                            let listener = TcpListener::bind(target)?;
                            listener.set_nonblocking(true)?;
                            Some(listener)
                        } else {
                            None
                        };
                        Ok(Link {
                            target,
                            listener,
                            stream: None,
                            next_attempt: now,
                        })
                    })
                    .collect::<io::Result<_>>()?;
                Ok(Self::Tcp {
                    links,
                    frame: Vec::new(),
                })
            }
            #[cfg(feature = "ros2")]
            Transport::Ros2 => {
                let topics: Vec<String> = (0..config.streams)
                    .map(|stream| config.stream_topic(stream))
                    .collect();
                let publishers = crate::ros2::Publishers::open(&topics, config.camera.is_some())?;
                if let Some(limit) = config.wait_for_subscribers {
                    publishers.wait_for_subscribers(limit)?;
                }
                Ok(Self::Ros2(Box::new(publishers)))
            }
            #[cfg(not(feature = "ros2"))]
            Transport::Ros2 => Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "this kapto-loadgen was built without ROS 2 (the ros2 feature)",
            )),
        }
    }

    /// Sends `sample` on stream `stream`; whether it went.
    pub(crate) fn send(&mut self, stream: usize, sample: &Sample) -> bool {
        match self {
            Self::Udp {
                socket,
                targets,
                message,
            } => {
                sample.encode(message);
                targets
                    .get(stream)
                    .is_some_and(|target| socket.send_to(message, target).is_ok())
            }
            Self::Tcp { links, frame } => {
                let Some(link) = links.get_mut(stream) else {
                    return false;
                };
                let mut message = [0; payload::SIZE];
                sample.encode(&mut message);
                frame.clear();
                let length = u32::try_from(message.len()).unwrap_or(u32::MAX);
                frame.extend_from_slice(&length.to_le_bytes());
                frame.extend_from_slice(&message);
                link.send(frame)
            }
            #[cfg(feature = "ros2")]
            Self::Ros2(publishers) => publishers.send(stream, sample),
        }
    }
}

impl Output {
    /// Sends a camera's `frame` on stream `stream`, stamped `source_time_ns`
    /// over ROS 2; whether it went. Never over UDP, which a frame doesn't
    /// fit.
    #[cfg_attr(
        not(feature = "ros2"),
        allow(unused_variables, reason = "only ROS 2 stamps a frame")
    )]
    pub(crate) fn send_frame(&mut self, stream: usize, frame: &[u8], source_time_ns: u64) -> bool {
        match self {
            Self::Udp { .. } => false,
            Self::Tcp {
                links,
                frame: framed,
            } => {
                let Some(link) = links.get_mut(stream) else {
                    return false;
                };
                framed.clear();
                let length = u32::try_from(frame.len()).unwrap_or(u32::MAX);
                framed.extend_from_slice(&length.to_le_bytes());
                framed.extend_from_slice(frame);
                link.send(framed)
            }
            #[cfg(feature = "ros2")]
            Self::Ros2(publishers) => publishers.send_frame(stream, frame, source_time_ns),
        }
    }
}

impl Link {
    fn send(&mut self, frame: &[u8]) -> bool {
        let due = self.listener.is_some() || Instant::now() >= self.next_attempt;
        if self.stream.is_none() && due {
            self.stream = self.establish();
            if self.stream.is_none() {
                self.next_attempt = Instant::now() + RETRY_EVERY;
            }
        }
        let Some(stream) = &mut self.stream else {
            return false;
        };
        if stream.write_all(frame).is_ok() {
            return true;
        }
        // The receiver went away; try again in a while.
        self.stream = None;
        self.next_attempt = Instant::now() + RETRY_EVERY;
        false
    }

    /// A new connection: connected to the target, or accepted from it.
    fn establish(&self) -> Option<TcpStream> {
        let stream = match &self.listener {
            None => TcpStream::connect_timeout(&self.target, CONNECT_TIMEOUT).ok()?,
            // Nothing waiting is the usual answer, and no different from
            // a failed accept: there is no connection yet.
            Some(listener) => listener.accept().ok()?.0,
        };
        stream.set_nonblocking(false).ok()?;
        // Each message as it is sent, as a robot's control loop would.
        stream.set_nodelay(true).ok()?;
        Some(stream)
    }
}
