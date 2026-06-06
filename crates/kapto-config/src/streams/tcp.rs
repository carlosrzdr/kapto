// SPDX-License-Identifier: Apache-2.0

//! The `tcp` table of a stream: which side connects, and how messages are
//! cut out of the byte stream.
//!
//! ```toml
//! [streams.base_odometry.tcp]
//! connect = "192.168.1.10:7000"
//!
//! [streams.base_odometry.tcp.framing]
//! kind = "length_prefix"
//! length_bytes = 4
//! byte_order = "big"
//! ```

use std::net::SocketAddr;

use toml_edit::TableLike;

use super::{ByteOrder, MAX_DATAGRAM_BYTES, MAX_OFFSET, Reader};

/// Where a stream arrives over TCP.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TcpDeclaration {
    /// Which side opens the connection.
    pub endpoint: TcpEndpointDeclaration,
    /// How messages are cut out of the byte stream.
    pub framing: FramingDeclaration,
    /// The first wait before connecting again after a failure, in
    /// milliseconds, if declared.
    pub reconnect_initial_ms: Option<u32>,
    /// The longest wait between attempts to connect, in milliseconds, if
    /// declared.
    pub reconnect_max_ms: Option<u32>,
}

/// Which side opens a TCP connection.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TcpEndpointDeclaration {
    /// `connect = "…"`: Kapto connects to the robot at this address.
    Connect(SocketAddr),
    /// `listen = "…"`: Kapto listens here and the robot connects.
    Listen(SocketAddr),
}

/// How the robot marks where each message ends (`framing.kind`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FramingDeclaration {
    /// `"length_prefix"`: each message starts with a header holding its
    /// length.
    LengthPrefix {
        /// Bytes in the header; by default, up to the end of the length.
        header_bytes: u16,
        /// Where the length is in the header; 0 by default.
        length_offset: u16,
        /// Size of the length: 1, 2, 4 or 8 bytes.
        length_bytes: u8,
        /// Byte order of the length; little-endian by default.
        byte_order: ByteOrder,
        /// Whether the length counts the header too; `false` by default.
        includes_header: bool,
    },
    /// `"delimiter"`: each message ends with these bytes, e.g. `"\n"`.
    Delimiter(Vec<u8>),
    /// `"fixed"`: every message has exactly this many bytes.
    Fixed(u16),
}

pub(super) const TCP_KEYS: &[&str] = &[
    "connect",
    "listen",
    "framing",
    "reconnect_initial_ms",
    "reconnect_max_ms",
];
pub(super) const FRAMING_KEYS: &[&str] = &[
    "kind",
    "length_bytes",
    "length_offset",
    "header_bytes",
    "byte_order",
    "includes_header",
    "delimiter",
    "size",
];
/// The longest wait between attempts to connect: an hour.
const MAX_RECONNECT_MS: i64 = 3_600_000;

#[derive(Clone, Copy)]
enum Kind {
    LengthPrefix,
    Delimiter,
    Fixed,
}

impl Kind {
    fn keys(self) -> &'static [&'static str] {
        match self {
            Self::LengthPrefix => &[
                "length_bytes",
                "length_offset",
                "header_bytes",
                "byte_order",
                "includes_header",
            ],
            Self::Delimiter => &["delimiter"],
            Self::Fixed => &["size"],
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::LengthPrefix => "length_prefix",
            Self::Delimiter => "delimiter",
            Self::Fixed => "fixed",
        }
    }
}

impl Reader<'_> {
    pub(super) fn tcp(
        &mut self,
        table: &dyn TableLike,
        stream_table: &dyn TableLike,
        stream: &str,
    ) -> Option<TcpDeclaration> {
        let path = format!("{stream}.tcp");
        self.unknown_keys(table, &path, TCP_KEYS);
        let address = "an address and port, e.g. \"192.168.1.10:7000\"";
        let connect = self.parsed::<SocketAddr>(table, &path, "connect", address);
        let listen = self.parsed::<SocketAddr>(table, &path, "listen", address);
        let endpoint = match (table.get("connect"), table.get("listen")) {
            (Some(_), Some(_)) => {
                let detail = format!(
                    "`{path}` has both `connect` and `listen`: Kapto either connects to the robot or listens for it"
                );
                self.invalid(table, "listen", detail);
                None
            }
            (None, None) => {
                let detail = format!("`{path}` needs `connect` or `listen`");
                self.invalid(stream_table, "tcp", detail);
                None
            }
            _ => connect
                .map(TcpEndpointDeclaration::Connect)
                .or(listen.map(TcpEndpointDeclaration::Listen)),
        };
        if let Some(
            TcpEndpointDeclaration::Connect(address) | TcpEndpointDeclaration::Listen(address),
        ) = endpoint
            && address.port() == 0
        {
            let key = if connect.is_some() {
                "connect"
            } else {
                "listen"
            };
            let detail = format!("`{path}.{key}` needs a port from 1 to 65535");
            self.invalid(table, key, detail);
            return None;
        }

        let framing = self.table(table, &format!("{path}."), "framing");
        let framing = if let Some(framing) = framing {
            self.framing(framing, table, &path)
        } else {
            if table.get("framing").is_none() {
                let detail = format!(
                    "`{path}` needs `framing`: how the robot marks where each message ends"
                );
                self.invalid(stream_table, "tcp", detail);
            }
            None
        };
        let range = 1..=MAX_RECONNECT_MS;
        let initial = self.integer(table, &path, "reconnect_initial_ms", range.clone());
        let max = self.integer(table, &path, "reconnect_max_ms", range);
        if let (Some(initial), Some(max)) = (initial, max)
            && initial > max
        {
            let detail = format!(
                "`{path}.reconnect_max_ms` ({max}) is shorter than `reconnect_initial_ms` ({initial})"
            );
            self.invalid(table, "reconnect_max_ms", detail);
        }
        Some(TcpDeclaration {
            endpoint: endpoint?,
            framing: framing?,
            reconnect_initial_ms: initial.and_then(|ms| u32::try_from(ms).ok()),
            reconnect_max_ms: max.and_then(|ms| u32::try_from(ms).ok()),
        })
    }

    fn framing(
        &mut self,
        table: &dyn TableLike,
        tcp_table: &dyn TableLike,
        tcp: &str,
    ) -> Option<FramingDeclaration> {
        let path = format!("{tcp}.framing");
        self.unknown_keys(table, &path, FRAMING_KEYS);
        let kinds =
            [Kind::LengthPrefix, Kind::Delimiter, Kind::Fixed].map(|kind| (kind.name(), kind));
        let Some(kind) = self.choice(table, &path, "kind", &kinds) else {
            if table.get("kind").is_none() {
                self.invalid(tcp_table, "framing", format!("`{path}` needs `kind`"));
            }
            return None;
        };
        let mut misplaced = false;
        for (key, _) in table.iter() {
            let belongs = key == "kind" || kind.keys().contains(&key);
            if !belongs && FRAMING_KEYS.contains(&key) {
                let detail = format!(
                    "`{path}.{key}` doesn't apply to kind = \"{}\", which takes {}",
                    kind.name(),
                    list(kind.keys())
                );
                self.invalid(table, key, detail);
                misplaced = true;
            }
        }
        let framing = match kind {
            Kind::LengthPrefix => self.length_prefix(table, tcp_table, &path),
            Kind::Delimiter => {
                let delimiter =
                    self.parsed::<String>(table, &path, "delimiter", "text, e.g. \"\\n\"");
                match delimiter {
                    Some(delimiter) if delimiter.is_empty() => {
                        let detail = format!("`{path}.delimiter` can't be empty");
                        self.invalid(table, "delimiter", detail);
                        None
                    }
                    Some(delimiter) => Some(FramingDeclaration::Delimiter(delimiter.into_bytes())),
                    None => {
                        self.missing(table, tcp_table, &path, "delimiter");
                        None
                    }
                }
            }
            Kind::Fixed => {
                let size = self.integer(table, &path, "size", 1..=i64::from(MAX_DATAGRAM_BYTES));
                if size.is_none() {
                    self.missing(table, tcp_table, &path, "size");
                }
                size.and_then(|size| u16::try_from(size).ok())
                    .map(FramingDeclaration::Fixed)
            }
        };
        if misplaced { None } else { framing }
    }

    fn length_prefix(
        &mut self,
        table: &dyn TableLike,
        tcp_table: &dyn TableLike,
        path: &str,
    ) -> Option<FramingDeclaration> {
        let length_bytes = self.integer(table, path, "length_bytes", 1..=8);
        if length_bytes.is_none() {
            self.missing(table, tcp_table, path, "length_bytes");
        }
        let length_bytes = match length_bytes {
            Some(bytes @ (1 | 2 | 4 | 8)) => Some(bytes),
            Some(_) => {
                let detail = format!("`{path}.length_bytes` must be 1, 2, 4 or 8");
                self.invalid(table, "length_bytes", detail);
                None
            }
            None => None,
        };
        let offset = self.integer(table, path, "length_offset", 0..=MAX_OFFSET);
        let header = self.integer(table, path, "header_bytes", 1..=MAX_OFFSET);
        let byte_order = self.choice(
            table,
            path,
            "byte_order",
            &[("little", ByteOrder::Little), ("big", ByteOrder::Big)],
        );
        let includes_header = self.boolean(table, path, "includes_header");
        let invalid = |key: &str, value: bool| table.get(key).is_some() && !value;
        if invalid("length_offset", offset.is_some())
            || invalid("header_bytes", header.is_some())
            || invalid("byte_order", byte_order.is_some())
            || invalid("includes_header", includes_header.is_some())
        {
            return None;
        }
        let length_bytes = length_bytes?;
        let offset = offset.unwrap_or(0);
        let header = header.unwrap_or(offset + length_bytes);
        if offset + length_bytes > header {
            let detail = format!(
                "`{path}.header_bytes` is {header}, but the length ends at byte {}",
                offset + length_bytes
            );
            self.invalid(table, "header_bytes", detail);
            return None;
        }
        Some(FramingDeclaration::LengthPrefix {
            header_bytes: u16::try_from(header).ok()?,
            length_offset: u16::try_from(offset).ok()?,
            length_bytes: u8::try_from(length_bytes).ok()?,
            byte_order: byte_order.unwrap_or(ByteOrder::Little),
            includes_header: includes_header.unwrap_or(false),
        })
    }

    /// A required framing key that is absent, reported at the `framing` line.
    fn missing(&mut self, table: &dyn TableLike, tcp_table: &dyn TableLike, path: &str, key: &str) {
        if table.get(key).is_none() {
            self.invalid(tcp_table, "framing", format!("`{path}` needs `{key}`"));
        }
    }

    /// `true` or `false`, or `None` if absent or invalid (an error).
    fn boolean(&mut self, table: &dyn TableLike, path: &str, name: &str) -> Option<bool> {
        let item = table.get(name)?;
        let value = item.as_bool();
        if value.is_none() {
            self.invalid(
                table,
                name,
                format!("`{path}.{name}` must be true or false"),
            );
        }
        value
    }
}

/// `a`, `b` and `c`.
fn list(keys: &[&str]) -> String {
    let quoted: Vec<String> = keys.iter().map(|key| format!("`{key}`")).collect();
    match quoted.split_last() {
        Some((last, rest)) if !rest.is_empty() => format!("{} and {last}", rest.join(", ")),
        _ => quoted.concat(),
    }
}
