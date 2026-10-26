// SPDX-License-Identifier: Apache-2.0

//! Where messages go: UDP datagrams, frames on TCP connections, or ROS 2
//! messages on topics (the `ros2` module, built with the `ros2` feature).
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
//! connection at each message; connecting, it tries again at the next
//! message when it was refused (at once: nothing listens there yet), and at
//! most once a second when it timed out, so a missing receiver doesn't hold
//! up the other streams. A receiver that takes its frames slowly holds the
//! stream up, as TCP holds up a robot, but never past a stop.

use std::io::{self, Write};
use std::net::{SocketAddr, TcpListener, TcpStream, UdpSocket};
use std::sync::atomic::{AtomicBool, Ordering};
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

/// How long a stream whose connection attempt timed out, or whose
/// receiver went away, waits before trying again.
const RETRY_EVERY: Duration = Duration::from_secs(1);

/// How long one write may wait on a receiver that isn't reading before
/// the stop is looked at again: a write waiting with no limit would keep
/// a stopped run from ever ending.
const WRITE_WAIT: Duration = Duration::from_millis(100);

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
                let kind = match config.camera {
                    None => crate::ros2::Kind::Joints,
                    Some((width, height)) if config.raw_camera => {
                        crate::ros2::Kind::Raw(width, height)
                    }
                    Some(_) => crate::ros2::Kind::Compressed,
                };
                let publishers = crate::ros2::Publishers::open(&topics, kind)?;
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

    /// Sends `sample` on stream `stream`, unless `stop` is set while it
    /// waits on its receiver; whether it went.
    pub(crate) fn send(&mut self, stream: usize, sample: &Sample, stop: &AtomicBool) -> bool {
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
                link.send(frame, stop)
            }
            #[cfg(feature = "ros2")]
            Self::Ros2(publishers) => publishers.send(stream, sample),
        }
    }
}

impl Output {
    /// Sends a camera's `frame` on stream `stream`, stamped `source_time_ns`
    /// over ROS 2, unless `stop` is set while it waits on its receiver;
    /// whether it went. Never over UDP, which a frame doesn't fit.
    #[cfg_attr(
        not(feature = "ros2"),
        allow(unused_variables, reason = "only ROS 2 stamps a frame")
    )]
    pub(crate) fn send_frame(
        &mut self,
        stream: usize,
        frame: &[u8],
        source_time_ns: u64,
        stop: &AtomicBool,
    ) -> bool {
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
                link.send(framed, stop)
            }
            #[cfg(feature = "ros2")]
            Self::Ros2(publishers) => publishers.send_frame(stream, frame, source_time_ns),
        }
    }
}

impl Link {
    fn send(&mut self, frame: &[u8], stop: &AtomicBool) -> bool {
        let due = self.listener.is_some() || Instant::now() >= self.next_attempt;
        if self.stream.is_none() && due {
            match self.establish() {
                Ok(stream) => self.stream = Some(stream),
                // Refused at once: the receiver isn't listening yet, and
                // asking again next time costs nothing.
                Err(error) if error.kind() == io::ErrorKind::ConnectionRefused => {}
                Err(_) => self.next_attempt = Instant::now() + RETRY_EVERY,
            }
        }
        let Some(stream) = &mut self.stream else {
            return false;
        };
        if write_unless_stopped(stream, frame, stop) {
            return true;
        }
        // The receiver went away, or the run stopped with the frame half
        // sent; try again in a while.
        self.stream = None;
        self.next_attempt = Instant::now() + RETRY_EVERY;
        false
    }

    /// A new connection: connected to the target, or accepted from it.
    fn establish(&self) -> io::Result<TcpStream> {
        let stream = match &self.listener {
            None => TcpStream::connect_timeout(&self.target, CONNECT_TIMEOUT)?,
            // Nothing waiting is the usual answer, and no different from
            // a failed accept: there is no connection yet.
            Some(listener) => listener.accept()?.0,
        };
        refuse_itself(&stream)?;
        stream.set_nonblocking(false)?;
        stream.set_write_timeout(Some(WRITE_WAIT))?;
        // Each message as it is sent, as a robot's control loop would.
        stream.set_nodelay(true)?;
        Ok(stream)
    }
}

/// Refuses a connection the generator made to itself. Connecting again and
/// again to a port in the ephemeral range where nothing listens (Kapto's
/// capture stopped), the system can pick that very port as the
/// connection's own, and TCP then connects it to itself: nothing would ever
/// read what it sends, and Kapto couldn't listen there again. Refused, it
/// is tried again from another port, as when nothing listens.
fn refuse_itself(stream: &TcpStream) -> io::Result<()> {
    if stream.local_addr()? == stream.peer_addr()? {
        return Err(io::Error::new(
            io::ErrorKind::ConnectionRefused,
            "connected to itself, with nothing listening",
        ));
    }
    Ok(())
}

/// Writes all of `frame` to `stream`, waiting on a receiver slow to read
/// for as long as it takes, unless `stop` is set meanwhile; whether all of
/// it went.
fn write_unless_stopped(stream: &mut TcpStream, mut frame: &[u8], stop: &AtomicBool) -> bool {
    while !frame.is_empty() {
        match stream.write(frame) {
            Ok(0) => return false,
            Ok(written) => frame = frame.get(written..).unwrap_or_default(),
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                ) =>
            {
                if stop.load(Ordering::Relaxed) {
                    return false;
                }
            }
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(_) => return false,
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_write_to_a_receiver_that_doesnt_read_gives_up_once_stopped() {
        let receiver = TcpListener::bind("127.0.0.1:0").unwrap();
        let mut sender = TcpStream::connect(receiver.local_addr().unwrap()).unwrap();
        let _never_read = receiver.accept().unwrap();
        sender.set_write_timeout(Some(WRITE_WAIT)).unwrap();
        // More than both sides' buffers hold.
        let frame = vec![0; 64 << 20];
        let stop = AtomicBool::new(false);
        let (went, waited) = std::thread::scope(|scope| {
            let writing = scope.spawn(|| write_unless_stopped(&mut sender, &frame, &stop));
            std::thread::sleep(Duration::from_millis(500));
            let stopped = Instant::now();
            stop.store(true, Ordering::Relaxed);
            (writing.join().unwrap(), stopped.elapsed())
        });
        assert!(!went);
        assert!(waited < Duration::from_secs(2), "{waited:?}");
    }

    #[test]
    fn a_write_to_a_receiver_that_reads_goes_whole() {
        let receiver = TcpListener::bind("127.0.0.1:0").unwrap();
        let mut sender = TcpStream::connect(receiver.local_addr().unwrap()).unwrap();
        let (mut reading, _) = receiver.accept().unwrap();
        sender.set_write_timeout(Some(WRITE_WAIT)).unwrap();
        let frame = vec![7; 8 << 20];
        let read = std::thread::spawn(move || {
            let mut all = Vec::new();
            io::Read::read_to_end(&mut reading, &mut all).map(|_| all)
        });
        assert!(write_unless_stopped(
            &mut sender,
            &frame,
            &AtomicBool::new(false)
        ));
        drop(sender);
        assert_eq!(read.join().unwrap().unwrap(), frame);
    }

    #[test]
    fn a_connection_to_itself_is_refused_and_one_to_a_receiver_is_not() {
        use socket2::{Domain, Socket, Type};
        let socket = Socket::new(Domain::IPV4, Type::STREAM, None).unwrap();
        let any: SocketAddr = "127.0.0.1:0".parse().unwrap();
        socket.bind(&any.into()).unwrap();
        let own = socket.local_addr().unwrap();
        socket.connect(&own).unwrap();
        let itself: TcpStream = socket.into();
        let error = refuse_itself(&itself).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::ConnectionRefused);

        let receiver = TcpListener::bind("127.0.0.1:0").unwrap();
        let other = TcpStream::connect(receiver.local_addr().unwrap()).unwrap();
        assert!(refuse_itself(&other).is_ok());
    }
}
