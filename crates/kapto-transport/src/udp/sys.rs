// SPDX-License-Identifier: Apache-2.0

//! The system calls the UDP receiver makes itself, because neither the
//! standard library nor `socket2` offers them: waiting on several sockets at
//! once, and receiving many datagrams in one call (`recvmmsg`).
//!
//! On Linux the wait is `epoll`, through rustix's safe functions: the
//! sockets are registered once, and each wait costs the same however many
//! there are, where `poll` looks at every one each time (about 0.07 µs a
//! socket, which adds up at 64 streams and more). Elsewhere it is `poll`.

#![allow(
    unsafe_code,
    reason = "recvmmsg, and poll outside Linux, are only reachable through libc's unsafe functions"
)]

#[cfg(not(target_os = "linux"))]
use std::fmt;
use std::io;
use std::net::{IpAddr, UdpSocket};
use std::os::fd::AsRawFd;
use std::time::Duration;

/// The most datagrams read by one call.
pub(super) const CHUNK: usize = 64;

/// What was received into one slot.
#[derive(Clone, Copy, Debug, Default)]
pub(super) struct Datagram {
    pub(super) len: usize,
    /// The datagram was longer than the slot and its end was cut off.
    pub(super) truncated: bool,
    pub(super) source: Option<IpAddr>,
}

/// The most sockets one wait reports as readable; any others are reported
/// by the next, since a socket stays readable until it is read.
#[cfg(target_os = "linux")]
const EVENTS: usize = 64;

/// The sockets to wait on, and which were readable after the last wait.
#[cfg(target_os = "linux")]
#[derive(Debug)]
pub(super) struct Readiness {
    epoll: rustix::fd::OwnedFd,
    /// Each socket, in order: whether the last wait found it readable.
    readable: Vec<bool>,
}

#[cfg(target_os = "linux")]
impl Readiness {
    /// Registers `sockets` to be waited on, each under its position.
    pub(super) fn new<'a>(sockets: impl Iterator<Item = &'a UdpSocket>) -> io::Result<Self> {
        use rustix::event::epoll::{self, CreateFlags, EventData, EventFlags};

        let epoll = epoll::create(CreateFlags::CLOEXEC)?;
        let mut readable = Vec::new();
        for (index, socket) in (0_u64..).zip(sockets) {
            epoll::add(&epoll, socket, EventData::new_u64(index), EventFlags::IN)?;
            readable.push(false);
        }
        Ok(Self { epoll, readable })
    }

    /// Waits until a socket is readable or `timeout` passes. Returns how many
    /// sockets are readable.
    pub(super) fn wait(&mut self, timeout: Duration) -> io::Result<usize> {
        use std::mem::MaybeUninit;

        use rustix::event::{Timespec, epoll};

        self.readable.fill(false);
        let timeout = Timespec {
            tv_sec: timeout.as_secs().try_into().unwrap_or(i64::MAX),
            tv_nsec: timeout.subsec_nanos().into(),
        };
        // On the stack: waiting allocates nothing.
        let mut events = [const { MaybeUninit::<epoll::Event>::uninit() }; EVENTS];
        let (ready, _) = match epoll::wait(&self.epoll, &mut events[..], Some(&timeout)) {
            Ok(waited) => waited,
            Err(error) => return interrupted_is_nothing(error.into()),
        };
        // An error on a socket is reported as an event too: it counts as
        // readable, and the next read returns the error.
        for event in ready.iter() {
            let index = usize::try_from(event.data.u64()).unwrap_or(usize::MAX);
            if let Some(readable) = self.readable.get_mut(index) {
                *readable = true;
            }
        }
        Ok(ready.len())
    }

    /// Whether each socket, in order, has something to read (or an error to
    /// report, which the next read returns).
    pub(super) fn readable(&self) -> impl Iterator<Item = bool> + '_ {
        self.readable.iter().copied()
    }
}

/// The sockets to wait on, and which were readable after the last wait.
#[cfg(not(target_os = "linux"))]
pub(super) struct Readiness(Vec<libc::pollfd>);

#[cfg(not(target_os = "linux"))]
impl Readiness {
    /// Lists `sockets` to be waited on, in order.
    pub(super) fn new<'a>(sockets: impl Iterator<Item = &'a UdpSocket>) -> io::Result<Self> {
        Ok(Self(
            sockets
                .map(|socket| libc::pollfd {
                    fd: socket.as_raw_fd(),
                    events: libc::POLLIN,
                    revents: 0,
                })
                .collect(),
        ))
    }

    /// Waits until a socket is readable or `timeout` passes. Returns how many
    /// sockets are readable.
    pub(super) fn wait(&mut self, timeout: Duration) -> io::Result<usize> {
        let millis = timeout.as_micros().div_ceil(1_000);
        let millis = libc::c_int::try_from(millis).unwrap_or(libc::c_int::MAX);
        let count = libc::nfds_t::try_from(self.0.len()).unwrap_or(libc::nfds_t::MAX);
        // SAFETY: the pointer and count describe `self.0`, which is
        // exclusively borrowed for the call; poll only writes `revents`.
        let ready = unsafe { libc::poll(self.0.as_mut_ptr(), count, millis) };
        match usize::try_from(ready) {
            Ok(ready) => Ok(ready),
            Err(_) => interrupted_is_nothing(io::Error::last_os_error()),
        }
    }

    /// Whether each socket, in order, has something to read (or an error to
    /// report, which the next read returns).
    pub(super) fn readable(&self) -> impl Iterator<Item = bool> + '_ {
        self.0.iter().map(|fd| fd.revents != 0)
    }
}

#[cfg(not(target_os = "linux"))]
impl fmt::Debug for Readiness {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Readiness")
            .field("sockets", &self.0.len())
            .finish()
    }
}

/// Reads the datagrams waiting on `socket`, without waiting, into
/// consecutive `slot_len`-byte slots of `slots`, describing each in `out`.
/// Returns how many were read.
#[cfg(target_os = "linux")]
pub(super) fn receive(
    socket: &UdpSocket,
    slots: &mut [u8],
    slot_len: usize,
    out: &mut [Datagram; CHUNK],
) -> io::Result<usize> {
    #[allow(clippy::cast_possible_truncation, reason = "the size is 128 bytes")]
    const ADDRESS_LEN: libc::socklen_t = size_of::<libc::sockaddr_storage>() as libc::socklen_t;

    let count = slots.len().checked_div(slot_len).unwrap_or(0).min(CHUNK);
    if count == 0 {
        return Ok(0);
    }
    // SAFETY: these are C structs of integers and pointers, for which all
    // zero bytes is a valid value.
    let (mut addresses, mut iovecs, mut headers) = unsafe {
        std::mem::zeroed::<(
            [libc::sockaddr_storage; CHUNK],
            [libc::iovec; CHUNK],
            [libc::mmsghdr; CHUNK],
        )>()
    };
    for (iovec, slot) in iovecs.iter_mut().zip(slots.chunks_exact_mut(slot_len)) {
        iovec.iov_base = slot.as_mut_ptr().cast();
        iovec.iov_len = slot_len;
    }
    for ((header, iovec), address) in headers.iter_mut().zip(&mut iovecs).zip(&mut addresses) {
        header.msg_hdr.msg_name = std::ptr::from_mut(address).cast();
        header.msg_hdr.msg_namelen = ADDRESS_LEN;
        header.msg_hdr.msg_iov = iovec;
        header.msg_hdr.msg_iovlen = 1;
    }
    let vlen = libc::c_uint::try_from(count).unwrap_or(0);
    // SAFETY: the first `count` headers each point to one address buffer of
    // the length given and one iovec covering a distinct `slot_len`-byte slot
    // inside `slots` (`count` slots fit in it). All of them outlive the call
    // and nothing else touches them during it. MSG_DONTWAIT makes the call
    // return at once, and a null timeout is allowed.
    let received = unsafe {
        libc::recvmmsg(
            socket.as_raw_fd(),
            headers.as_mut_ptr(),
            vlen,
            libc::MSG_DONTWAIT,
            std::ptr::null_mut(),
        )
    };
    let Ok(received) = usize::try_from(received) else {
        return interrupted_is_nothing(io::Error::last_os_error());
    };
    for ((datagram, header), address) in out.iter_mut().zip(&headers).zip(&addresses).take(received)
    {
        *datagram = Datagram {
            len: header.msg_len as usize,
            truncated: header.msg_hdr.msg_flags & libc::MSG_TRUNC != 0,
            source: source_ip(address),
        };
    }
    Ok(received)
}

/// Reads the datagrams waiting on `socket`, without waiting, into
/// consecutive `slot_len`-byte slots of `slots`, describing each in `out`.
/// Returns how many were read. This fallback can't tell that a datagram was
/// cut to fit its slot.
#[cfg(not(target_os = "linux"))]
pub(super) fn receive(
    socket: &UdpSocket,
    slots: &mut [u8],
    slot_len: usize,
    out: &mut [Datagram; CHUNK],
) -> io::Result<usize> {
    let mut received = 0;
    for (slot, datagram) in slots.chunks_exact_mut(slot_len.max(1)).zip(out) {
        match socket.recv_from(slot) {
            Ok((len, source)) => {
                *datagram = Datagram {
                    len,
                    truncated: false,
                    source: Some(source.ip()),
                };
                received += 1;
            }
            Err(error) => return interrupted_is_nothing(error).map(|_| received),
        }
    }
    Ok(received)
}

/// Having nothing to read, or being interrupted by a signal, means "nothing
/// yet"; anything else is a real failure.
fn interrupted_is_nothing(error: io::Error) -> io::Result<usize> {
    match error.kind() {
        io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted => Ok(0),
        _ => Err(error),
    }
}

#[cfg(target_os = "linux")]
fn source_ip(address: &libc::sockaddr_storage) -> Option<IpAddr> {
    match libc::c_int::from(address.ss_family) {
        libc::AF_INET => {
            // SAFETY: the family says the kernel wrote a sockaddr_in, which
            // fits in a sockaddr_storage and needs no stricter alignment.
            let address = unsafe { &*std::ptr::from_ref(address).cast::<libc::sockaddr_in>() };
            // `s_addr` holds the address in network byte order, as in memory.
            Some(IpAddr::from(address.sin_addr.s_addr.to_ne_bytes()))
        }
        libc::AF_INET6 => {
            // SAFETY: as above, for a sockaddr_in6.
            let address = unsafe { &*std::ptr::from_ref(address).cast::<libc::sockaddr_in6>() };
            Some(IpAddr::from(address.sin6_addr.s6_addr))
        }
        _ => None,
    }
}
