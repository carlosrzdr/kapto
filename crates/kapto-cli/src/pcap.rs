// SPDX-License-Identifier: Apache-2.0

//! UDP datagrams out of a packet capture (pcap or pcapng, as tcpdump and
//! Wireshark write them), so a robot's traffic can be decoded offline.
//!
//! The file formats are read with `pcap-parser`. The headers inside each
//! packet are read here: Ethernet (with VLAN tags), Linux cooked captures,
//! the BSD loopback header and raw IP, then IPv4 or IPv6, then UDP. Packets
//! that aren't UDP are passed over; fragments of a datagram too large for
//! one packet are counted, since without the rest they can't be read.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::ops::ControlFlow;

use kapto_core::{ErrorCode, KaptoError};
use pcap_parser::pcapng::Block;
use pcap_parser::{LegacyPcapSlice, Linktype, PcapBlockOwned, PcapNGSlice};

/// One UDP datagram in the capture.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Datagram<'a> {
    /// When it was captured, in UTC nanoseconds since the Unix epoch.
    pub(crate) time_ns: u64,
    /// Where it came from.
    pub(crate) source: SocketAddr,
    /// Where it went.
    pub(crate) destination: SocketAddr,
    /// Its payload.
    pub(crate) payload: &'a [u8],
}

/// Packets that weren't read as datagrams, by why.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Passed {
    /// Packets of another protocol, or of a link type this doesn't read.
    pub(crate) other: u64,
    /// Fragments of a datagram split over several packets.
    pub(crate) fragments: u64,
    /// Packets cut short when captured (a snapshot length too small).
    pub(crate) cut_short: u64,
}

/// The pcapng magic number: a section header block.
const PCAPNG_MAGIC: [u8; 4] = [0x0a, 0x0d, 0x0d, 0x0a];

/// Calls `each` with every UDP datagram in the capture `bytes`, in file
/// order, until it breaks.
///
/// # Errors
///
/// `decode.capture_invalid` if the file isn't a packet capture, or is
/// damaged before its end.
pub(crate) fn read_capture(
    bytes: &[u8],
    mut each: impl FnMut(Datagram<'_>) -> ControlFlow<()>,
) -> Result<Passed, KaptoError> {
    let invalid = |detail: String| KaptoError::new(ErrorCode::DecodeCaptureInvalid).detail(detail);
    let mut passed = Passed::default();
    let mut emit = |linktype: Linktype, time_ns: u64, data: &[u8], cut_short: bool| {
        if cut_short {
            passed.cut_short += 1;
            return ControlFlow::Continue(());
        }
        match datagram(linktype, data) {
            Read::Datagram {
                source,
                destination,
                payload,
            } => each(Datagram {
                time_ns,
                source,
                destination,
                payload,
            }),
            Read::Fragment => {
                passed.fragments += 1;
                ControlFlow::Continue(())
            }
            Read::Other => {
                passed.other += 1;
                ControlFlow::Continue(())
            }
        }
    };
    if bytes.starts_with(&PCAPNG_MAGIC) {
        let blocks = PcapNGSlice::from_slice(bytes)
            .map_err(|error| invalid(format!("not a pcapng file: {error:?}")))?;
        // Each interface's link type, and its clock: units per second and
        // offset in seconds.
        let mut interfaces: Vec<(Linktype, u64, i64)> = Vec::new();
        for block in blocks {
            let block = block.map_err(|error| invalid(format!("damaged: {error:?}")))?;
            let PcapBlockOwned::NG(block) = block else {
                continue;
            };
            match block {
                Block::SectionHeader(_) => interfaces.clear(),
                Block::InterfaceDescription(interface) => {
                    let resolution = interface.ts_resolution().unwrap_or(1_000_000);
                    interfaces.push((interface.linktype, resolution, interface.if_tsoffset));
                }
                Block::EnhancedPacket(packet) => {
                    let Some(&(linktype, resolution, offset)) = usize::try_from(packet.if_id)
                        .ok()
                        .and_then(|id| interfaces.get(id))
                    else {
                        return Err(invalid(format!(
                            "a packet names interface {}, which isn't described",
                            packet.if_id
                        )));
                    };
                    let ticks = (u64::from(packet.ts_high) << 32) | u64::from(packet.ts_low);
                    let time_ns = ns(ticks, resolution, offset);
                    let length = usize::try_from(packet.caplen).unwrap_or(usize::MAX);
                    let data = packet.data.get(..length).unwrap_or(packet.data);
                    let cut_short = packet.caplen < packet.origlen;
                    if emit(linktype, time_ns, data, cut_short).is_break() {
                        break;
                    }
                }
                _ => {}
            }
        }
    } else {
        let mut blocks = LegacyPcapSlice::from_slice(bytes)
            .map_err(|_| invalid("neither a pcap nor a pcapng file".to_owned()))?;
        let linktype = blocks.header.network;
        let per_fraction = if blocks.header.is_nanosecond_precision() {
            1
        } else {
            1_000
        };
        for block in blocks.by_ref() {
            let block = block.map_err(|error| invalid(format!("damaged: {error:?}")))?;
            let PcapBlockOwned::Legacy(packet) = block else {
                continue;
            };
            let time_ns =
                u64::from(packet.ts_sec) * 1_000_000_000 + u64::from(packet.ts_usec) * per_fraction;
            let cut_short = packet.caplen < packet.origlen;
            if emit(linktype, time_ns, packet.data, cut_short).is_break() {
                break;
            }
        }
    }
    Ok(passed)
}

/// Nanoseconds since the epoch of `ticks` at `resolution` per second, from
/// `offset` seconds.
fn ns(ticks: u64, resolution: u64, offset: i64) -> u64 {
    let since = u128::from(ticks) * 1_000_000_000 / u128::from(resolution.max(1));
    let offset = i128::from(offset) * 1_000_000_000;
    u64::try_from(i128::try_from(since).unwrap_or(i128::MAX) + offset).unwrap_or(0)
}

/// What a packet turned out to be.
enum Read<'a> {
    Datagram {
        source: SocketAddr,
        destination: SocketAddr,
        payload: &'a [u8],
    },
    Fragment,
    Other,
}

const ETHERTYPE_IPV4: u16 = 0x0800;
const ETHERTYPE_IPV6: u16 = 0x86dd;
const ETHERTYPE_VLAN: [u16; 2] = [0x8100, 0x88a8];
const UDP: u8 = 17;

/// The UDP datagram in one captured packet, if it holds one.
fn datagram(linktype: Linktype, data: &[u8]) -> Read<'_> {
    let ip = match linktype {
        Linktype::ETHERNET => ethernet(data),
        // Linux "cooked" capture (`tcpdump -i any`): protocol at 14.
        Linktype::LINUX_SLL => data.get(16..).zip(be16(data, 14)),
        // Its second version: protocol first.
        Linktype::LINUX_SLL2 => data.get(20..).zip(be16(data, 0)),
        Linktype::RAW | Linktype::IPV4 | Linktype::IPV6 => match data.first().map(|byte| byte >> 4)
        {
            Some(4) => Some((data, ETHERTYPE_IPV4)),
            Some(6) => Some((data, ETHERTYPE_IPV6)),
            _ => None,
        },
        // BSD loopback: the address family in the host's byte order, or
        // (LOOP) always big-endian.
        Linktype::NULL | Linktype::LOOP => {
            let family = data.get(..4).map(|bytes| {
                let raw = [bytes[0], bytes[1], bytes[2], bytes[3]];
                u32::from_le_bytes(raw).min(u32::from_be_bytes(raw))
            });
            match family {
                Some(2) => data.get(4..).map(|ip| (ip, ETHERTYPE_IPV4)),
                Some(24 | 28 | 30) => data.get(4..).map(|ip| (ip, ETHERTYPE_IPV6)),
                _ => None,
            }
        }
        _ => None,
    };
    match ip {
        Some((packet, ETHERTYPE_IPV4)) => ipv4(packet),
        Some((packet, ETHERTYPE_IPV6)) => ipv6(packet),
        _ => Read::Other,
    }
}

fn be16(data: &[u8], at: usize) -> Option<u16> {
    let bytes = data.get(at..at + 2)?;
    Some(u16::from_be_bytes([bytes[0], bytes[1]]))
}

/// The IP packet in an Ethernet frame, past any VLAN tags.
fn ethernet(frame: &[u8]) -> Option<(&[u8], u16)> {
    let mut at = 12;
    let mut ethertype = be16(frame, at)?;
    while ETHERTYPE_VLAN.contains(&ethertype) {
        at += 4;
        ethertype = be16(frame, at)?;
    }
    Some((frame.get(at + 2..)?, ethertype))
}

fn ipv4(packet: &[u8]) -> Read<'_> {
    let Some(&first) = packet.first() else {
        return Read::Other;
    };
    let header = usize::from(first & 0x0f) * 4;
    let (Some(total), Some(flags), Some(&protocol)) =
        (be16(packet, 2), be16(packet, 6), packet.get(9))
    else {
        return Read::Other;
    };
    if protocol != UDP || header < 20 {
        return Read::Other;
    }
    // More fragments, or not the first: part of a larger datagram.
    if flags & 0x2000 != 0 || flags & 0x1fff != 0 {
        return Read::Fragment;
    }
    let (Some(source), Some(destination)) = (packet.get(12..16), packet.get(16..20)) else {
        return Read::Other;
    };
    let address = |bytes: &[u8]| IpAddr::V4(Ipv4Addr::new(bytes[0], bytes[1], bytes[2], bytes[3]));
    // Ethernet pads short frames; the IP length says where the packet ends.
    let end = usize::from(total).min(packet.len());
    match packet.get(header..end) {
        Some(udp) => self::udp(udp, address(source), address(destination)),
        None => Read::Other,
    }
}

fn ipv6(packet: &[u8]) -> Read<'_> {
    let (Some(length), Some(&next)) = (be16(packet, 4), packet.get(6)) else {
        return Read::Other;
    };
    // Extension headers (a fragment header among them) aren't followed.
    if next != UDP {
        return Read::Other;
    }
    let (Some(source), Some(destination)) = (packet.get(8..24), packet.get(24..40)) else {
        return Read::Other;
    };
    let address = |bytes: &[u8]| {
        let mut octets = [0; 16];
        octets.copy_from_slice(bytes);
        IpAddr::V6(Ipv6Addr::from(octets))
    };
    let end = (40 + usize::from(length)).min(packet.len());
    match packet.get(40..end) {
        Some(udp) => self::udp(udp, address(source), address(destination)),
        None => Read::Other,
    }
}

fn udp(segment: &[u8], source: IpAddr, destination: IpAddr) -> Read<'_> {
    let (Some(from), Some(to), Some(length)) =
        (be16(segment, 0), be16(segment, 2), be16(segment, 4))
    else {
        return Read::Other;
    };
    match segment.get(8..usize::from(length)) {
        Some(payload) => Read::Datagram {
            source: SocketAddr::new(source, from),
            destination: SocketAddr::new(destination, to),
            payload,
        },
        None => Read::Other,
    }
}
