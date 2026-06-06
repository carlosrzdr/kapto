// SPDX-License-Identifier: Apache-2.0

//! The tap's datagram format, version 2: one stream's decoded values over a
//! span of time, with each value's newest reading, minimum and maximum, so a
//! plot drawn from far fewer samples than were recorded still shows every
//! spike the tap saw.
//!
//! | Offset | Bytes | Field |
//! |---|---|---|
//! | 0 | 4 | `KTAP` |
//! | 4 | 1 | version, 2 |
//! | 5 | 1 | 0, reserved |
//! | 6 | 2 | stream index, `u16`, its position in the stream declarations |
//! | 8 | 8 | receive time of the newest message, `u64` nanoseconds of the host's monotonic clock |
//! | 16 | 8 | receive time of the oldest message, the same way |
//! | 24 | 4 | messages summarised `m`, `u32` |
//! | 28 | 4 | value count `n`, `u32` |
//! | 32 | 8 × n | the newest message's values, `f64` |
//! | 32 + 8n | 8 × n | each value's minimum over the `m` messages, `f64` |
//! | 32 + 16n | 8 × n | each value's maximum over the `m` messages, `f64` |
//!
//! All numbers are little-endian. A reader skips datagrams of a version it
//! doesn't know, so versions can change without breaking older viewers.

/// The first four bytes of every datagram.
pub const MAGIC: [u8; 4] = *b"KTAP";

/// The version this code writes.
pub const VERSION: u8 = 2;

/// The bytes before the values.
pub const HEADER: usize = 32;

/// The most values one datagram carries, so it stays well under the
/// datagram size Unix sockets accept by default.
pub const MAX_VALUES: usize = 1024;

/// The largest datagram: a header and the most values, three times over.
pub const MAX_DATAGRAM: usize = HEADER + 3 * 8 * MAX_VALUES;

/// One stream's values over a span of time, as written.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Summary<'a> {
    /// The stream's position in the stream declarations.
    pub stream: u16,
    /// When the oldest message summarised was received, in nanoseconds of
    /// the host's monotonic clock.
    pub first_recv_time_ns: u64,
    /// When the newest one was received, the same way.
    pub recv_time_ns: u64,
    /// How many messages it summarises.
    pub messages: u32,
    /// The newest message's values, field after field.
    pub values: &'a [f64],
    /// Each value's minimum over those messages.
    pub minimums: &'a [f64],
    /// Each value's maximum over those messages.
    pub maximums: &'a [f64],
}

impl<'a> Summary<'a> {
    /// One message on its own: its values are their own minimum and
    /// maximum.
    #[must_use]
    pub fn single(stream: u16, recv_time_ns: u64, values: &'a [f64]) -> Self {
        Self {
            stream,
            first_recv_time_ns: recv_time_ns,
            recv_time_ns,
            messages: 1,
            values,
            minimums: values,
            maximums: values,
        }
    }
}

/// One stream's values over a span of time, as read.
#[derive(Clone, Debug, PartialEq)]
pub struct TapSample {
    /// The stream's position in the stream declarations.
    pub stream: u16,
    /// When the oldest message summarised was received, in nanoseconds of
    /// the host's monotonic clock.
    pub first_recv_time_ns: u64,
    /// When the newest one was received, the same way.
    pub recv_time_ns: u64,
    /// How many messages it summarises.
    pub messages: u32,
    /// The newest message's values, field after field.
    pub values: Vec<f64>,
    /// Each value's minimum over those messages.
    pub minimums: Vec<f64>,
    /// Each value's maximum over those messages.
    pub maximums: Vec<f64>,
}

impl TapSample {
    /// One message on its own: its values are their own minimum and
    /// maximum.
    #[must_use]
    pub fn single(stream: u16, recv_time_ns: u64, values: Vec<f64>) -> Self {
        Self {
            stream,
            first_recv_time_ns: recv_time_ns,
            recv_time_ns,
            messages: 1,
            minimums: values.clone(),
            maximums: values.clone(),
            values,
        }
    }
}

/// Why a datagram isn't a tap sample this code can read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WireError {
    /// It doesn't start with `KTAP`, or is too short for its header.
    NotTap,
    /// It is a tap datagram of another version.
    Version(u8),
    /// Its length doesn't match its value count.
    Length,
}

/// Writes one summary into `out`, replacing what it held. At most
/// [`MAX_VALUES`] values are written; minimums and maximums missing for a
/// value are written as the value itself.
pub fn encode(summary: &Summary<'_>, out: &mut Vec<u8>) {
    let count = summary.values.len().min(MAX_VALUES);
    out.clear();
    out.extend_from_slice(&MAGIC);
    out.extend_from_slice(&[VERSION, 0]);
    out.extend_from_slice(&summary.stream.to_le_bytes());
    out.extend_from_slice(&summary.recv_time_ns.to_le_bytes());
    out.extend_from_slice(&summary.first_recv_time_ns.to_le_bytes());
    out.extend_from_slice(&summary.messages.to_le_bytes());
    // At most MAX_VALUES, so it fits.
    out.extend_from_slice(&u32::try_from(count).unwrap_or(0).to_le_bytes());
    let values = &summary.values[..count];
    for column in [values, summary.minimums, summary.maximums] {
        for (n, value) in values.iter().enumerate() {
            out.extend_from_slice(&column.get(n).unwrap_or(value).to_le_bytes());
        }
    }
}

/// Reads one datagram.
///
/// # Errors
///
/// What makes it unreadable: not a tap datagram, another version, or a
/// length that doesn't match its value count.
pub fn decode(datagram: &[u8]) -> Result<TapSample, WireError> {
    let header = datagram.get(..HEADER).ok_or(WireError::NotTap)?;
    if header[..4] != MAGIC {
        return Err(WireError::NotTap);
    }
    if header[4] != VERSION {
        return Err(WireError::Version(header[4]));
    }
    let number = |range: std::ops::Range<usize>| {
        let mut bytes = [0; 8];
        bytes[..range.len()].copy_from_slice(&header[range]);
        u64::from_le_bytes(bytes)
    };
    let count = usize::try_from(number(28..32)).map_err(|_| WireError::Length)?;
    if count > MAX_VALUES || datagram.len() != HEADER + 3 * 8 * count {
        return Err(WireError::Length);
    }
    let column = |n: usize| -> Vec<f64> {
        let start = HEADER + n * 8 * count;
        datagram[start..start + 8 * count]
            .as_chunks::<8>()
            .0
            .iter()
            .map(|bytes| f64::from_le_bytes(*bytes))
            .collect()
    };
    Ok(TapSample {
        stream: u16::try_from(number(6..8)).unwrap_or(u16::MAX),
        recv_time_ns: number(8..16),
        first_recv_time_ns: number(16..24),
        messages: u32::try_from(number(24..28)).unwrap_or(u32::MAX),
        values: column(0),
        minimums: column(1),
        maximums: column(2),
    })
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;

    #[test]
    fn a_sample_has_exactly_the_documented_bytes() {
        let mut out = Vec::new();
        let summary = Summary {
            stream: 3,
            first_recv_time_ns: 0x0A0B,
            recv_time_ns: 0x0102_0304_0506_0708,
            messages: 5,
            values: &[1.5],
            minimums: &[-2.0],
            maximums: &[4.0],
        };
        encode(&summary, &mut out);
        let mut expected = b"KTAP\x02\x00\x03\x00".to_vec();
        expected.extend([8, 7, 6, 5, 4, 3, 2, 1]);
        expected.extend([0x0B, 0x0A, 0, 0, 0, 0, 0, 0]);
        expected.extend(5_u32.to_le_bytes());
        expected.extend(1_u32.to_le_bytes());
        expected.extend(1.5_f64.to_le_bytes());
        expected.extend((-2.0_f64).to_le_bytes());
        expected.extend(4.0_f64.to_le_bytes());
        assert_eq!(out, expected);
    }

    #[test]
    fn a_single_message_is_its_own_minimum_and_maximum() {
        let mut out = Vec::new();
        encode(&Summary::single(1, 7, &[0.5, -1.0]), &mut out);
        assert_eq!(
            decode(&out).unwrap(),
            TapSample::single(1, 7, vec![0.5, -1.0])
        );
    }

    #[test]
    fn datagrams_that_are_not_version_2_samples_are_refused() {
        let mut out = Vec::new();
        encode(&Summary::single(0, 0, &[1.0, 2.0]), &mut out);
        assert_eq!(decode(&out[..10]), Err(WireError::NotTap));
        assert_eq!(
            decode(b"HELLO, WORLD! THIS IS LONG ENOUGH FOR A HEADER"),
            Err(WireError::NotTap)
        );
        assert_eq!(decode(&out[..out.len() - 1]), Err(WireError::Length));
        let mut older = out.clone();
        older[4] = 1;
        assert_eq!(decode(&older), Err(WireError::Version(1)));
    }

    fn numbers(count: usize) -> impl Strategy<Value = Vec<f64>> {
        prop::collection::vec(any::<f64>().prop_filter("a number", |v| !v.is_nan()), count)
    }

    proptest! {
        #[test]
        fn samples_read_back_as_written(
            stream: u16,
            first: u64,
            time: u64,
            messages: u32,
            columns in (0..64_usize).prop_flat_map(|count| (numbers(count), numbers(count), numbers(count))),
        ) {
            let (values, minimums, maximums) = columns;
            let mut out = Vec::new();
            let summary = Summary {
                stream,
                first_recv_time_ns: first,
                recv_time_ns: time,
                messages,
                values: &values,
                minimums: &minimums,
                maximums: &maximums,
            };
            encode(&summary, &mut out);
            let sample = decode(&out).unwrap();
            prop_assert_eq!(sample, TapSample {
                stream,
                first_recv_time_ns: first,
                recv_time_ns: time,
                messages,
                values,
                minimums,
                maximums,
            });
        }
    }
}
