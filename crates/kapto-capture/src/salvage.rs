// SPDX-License-Identifier: Apache-2.0

//! Salvaging a recording cut off part-way, by a crash or a power cut.
//!
//! The writer closes a chunk and puts the file on disk on a timer, so a
//! recording cut off part-way holds every chunk written before the last
//! sync, perhaps a record written only in part after them, and none of the
//! records an MCAP file ends with. Salvaging cuts it at the end of its last
//! intact record, every chunk's checksum checked, and closes it as an MCAP
//! file closes: a data end record, a footer and the magic. Every message
//! kept stays exactly as received; what came after the last intact record,
//! at most what wasn't yet on disk, is lost, and its size is said.
//!
//! The closed file has no summary section, so readers read it from start
//! to end, as Kapto's do.

use std::fs::{File, OpenOptions};
use std::io::{self, BufReader, Cursor, Read, Seek, SeekFrom, Write};
use std::path::Path;

use kapto_core::{ErrorCode, KaptoError};
use mcap::records::{Record, op};
use mcap::sans_io::linear_reader::{LinearReadEvent, LinearReader, LinearReaderOptions};

use crate::reader::record_length_limit;

/// What salvaging a recording kept and cut.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Salvaged {
    /// Bytes kept from the start of the file, before the closing records.
    pub kept_bytes: u64,
    /// Bytes cut off its end: records written only in part, or not put on
    /// disk before it stopped.
    pub cut_bytes: u64,
    /// Messages kept.
    pub messages: u64,
    /// The newest message kept, in UTC nanoseconds, if any was.
    pub last_log_time: Option<u64>,
    /// The file was already a closed recording: nothing was changed.
    pub already_whole: bool,
}

/// A record's opcode and length before its content: one byte and eight.
const RECORD_HEADER: u64 = 9;

/// Salvages the recording at `path` in place: keeps it up to its last
/// intact record, then closes it, and says what was kept and cut. A file
/// that is already a closed recording is left as it is.
///
/// # Errors
///
/// `recording.unreadable` if it can't be read, doesn't start as an MCAP
/// recording, or holds no intact record to keep; `capture.write_failed` if
/// it can't be cut and closed.
pub fn salvage(path: &Path) -> Result<Salvaged, KaptoError> {
    let unreadable = |detail: String| {
        KaptoError::new(ErrorCode::RecordingUnreadable)
            .detail(detail)
            .context("file", path.display().to_string())
    };
    let length = std::fs::metadata(path)
        .map_err(|error| unreadable(error.to_string()))?
        .len();
    if let Ok(read) = read_prefix(path, length, false) {
        return Ok(Salvaged {
            kept_bytes: length,
            cut_bytes: 0,
            messages: read.messages,
            last_log_time: read.last_log_time,
            already_whole: true,
        });
    }
    let ends = record_ends(path, length).map_err(|error| unreadable(error.to_string()))?;
    // A prefix that reads cleanly once closed stays clean when shortened, so
    // the longest is found by halving: each try reads the prefix once.
    let (mut good, mut bad) = (None, ends.len());
    let mut low = 0;
    while low < bad {
        let middle = low + (bad - low) / 2;
        match read_prefix(path, ends[middle], true) {
            Ok(read) => {
                good = Some((ends[middle], read));
                low = middle + 1;
            }
            Err(_) => bad = middle,
        }
    }
    let Some((kept_bytes, read)) = good else {
        return Err(unreadable(
            "no intact record to keep: it was cut off before its first".to_owned(),
        ));
    };
    close(path, kept_bytes).map_err(|error| {
        KaptoError::new(ErrorCode::CaptureWriteFailed)
            .detail(format!("can't cut and close the recording: {error}"))
            .context("file", path.display().to_string())
    })?;
    Ok(Salvaged {
        kept_bytes,
        cut_bytes: length - kept_bytes,
        messages: read.messages,
        last_log_time: read.last_log_time,
        already_whole: false,
    })
}

/// Where each record at the top of the file ends, from its framing alone:
/// an opcode and a length. The first must be the header, which every MCAP
/// recording starts with; none is kept without it. Stops at a record
/// running past the end of the file, at an opcode of zero (disk blocks the
/// file grew into but that were never written), or at the data end record,
/// after which a file holds only its summary and footer.
fn record_ends(path: &Path, length: u64) -> io::Result<Vec<u64>> {
    let mut file = BufReader::new(File::open(path)?);
    let mut magic = [0_u8; 8];
    file.read_exact(&mut magic)?;
    if magic != mcap::MAGIC {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "it doesn't start as an MCAP recording",
        ));
    }
    let mut ends = Vec::new();
    let mut position = magic.len() as u64;
    let mut header = [0_u8; 9];
    while position + RECORD_HEADER <= length {
        file.read_exact(&mut header)?;
        let opcode = header[0];
        let mut size = [0_u8; 8];
        size.copy_from_slice(&header[1..]);
        let size = u64::from_le_bytes(size);
        let end = position.saturating_add(RECORD_HEADER).saturating_add(size);
        if ends.is_empty() && opcode != op::HEADER {
            break;
        }
        if opcode == 0 || opcode == op::DATA_END || opcode == op::FOOTER || end > length {
            break;
        }
        file.seek_relative(i64::try_from(size).unwrap_or(i64::MAX))?;
        position = end;
        ends.push(end);
    }
    Ok(ends)
}

/// What reading a recording found.
#[derive(Clone, Copy, Debug, Default)]
struct PrefixRead {
    messages: u64,
    last_log_time: Option<u64>,
}

/// Reads the first `length` bytes of the recording at `path`, closed with
/// the records salvaging adds when `closed`, every chunk's checksum checked:
/// what it holds, if every record in it is intact.
fn read_prefix(path: &Path, length: u64, closed: bool) -> Result<PrefixRead, String> {
    let file = File::open(path).map_err(|error| error.to_string())?;
    let closing = if closed {
        closing_records()
    } else {
        Vec::new()
    };
    // What is left to read: a reader asking for more is reading a record
    // whose length runs past the end, and is never given room for it.
    let mut left = length + closing.len() as u64;
    let mut source = file.take(length).chain(Cursor::new(closing));
    let mut reader = LinearReader::new_with_options(
        LinearReaderOptions::default()
            .with_prevalidate_chunk_crcs(true)
            .with_record_length_limit(record_length_limit(length)),
    );
    let mut read = PrefixRead::default();
    while let Some(event) = reader.next_event() {
        match event.map_err(|error| error.to_string())? {
            LinearReadEvent::ReadRequest(need) => {
                if left < need as u64 {
                    return Err("a record runs past the end of the file".to_owned());
                }
                let got = source
                    .read(reader.insert(need))
                    .map_err(|error| error.to_string())?;
                left -= got as u64;
                reader.notify_read(got);
            }
            LinearReadEvent::Record { opcode, data } => {
                if let Record::Message { header, .. } =
                    mcap::parse_record(opcode, data).map_err(|error| error.to_string())?
                {
                    read.messages += 1;
                    read.last_log_time = read.last_log_time.max(Some(header.log_time));
                }
            }
        }
    }
    Ok(read)
}

/// The records an MCAP file ends with, as salvaging writes them: a data end
/// record and a footer, neither with a checksum or a summary to point to,
/// then the magic.
fn closing_records() -> Vec<u8> {
    let mut bytes = Vec::with_capacity(13 + 29 + 8);
    // The data section's checksum: 0, not computed.
    bytes.push(op::DATA_END);
    bytes.extend(4_u64.to_le_bytes());
    bytes.extend(0_u32.to_le_bytes());
    // No summary section: where it and its offsets start, and its checksum.
    bytes.push(op::FOOTER);
    bytes.extend(20_u64.to_le_bytes());
    bytes.extend(0_u64.to_le_bytes());
    bytes.extend(0_u64.to_le_bytes());
    bytes.extend(0_u32.to_le_bytes());
    bytes.extend(mcap::MAGIC);
    bytes
}

/// Cuts the file at `path` to `length` bytes and closes it, then puts it on
/// disk.
fn close(path: &Path, length: u64) -> io::Result<()> {
    let mut file = OpenOptions::new().write(true).open(path)?;
    file.set_len(length)?;
    file.seek(SeekFrom::End(0))?;
    file.write_all(&closing_records())?;
    file.sync_all()
}
