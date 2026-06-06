// SPDX-License-Identifier: Apache-2.0

//! Payloads with a fixed binary layout: each field at a byte offset.
//!
//! A [`StructLayout`] is checked and compiled once into a [`StructDecoder`]:
//! one read per value, in output order, with its offset, type, byte order and
//! scale already worked out. Decoding a payload is then a loop over those
//! reads, with no lookups by name and no allocation.

use std::collections::BTreeSet;

use kapto_core::name::is_name;
use kapto_core::{ErrorCode, KaptoError};

use crate::Decoder;
use crate::field::{DecodeError, Endianness, FieldDescription, ScalarType, Value};

/// The largest UDP payload: no field may end beyond it.
pub const MAX_MESSAGE_BYTES: usize = 65_507;

/// One field of a fixed layout.
#[derive(Clone, Debug, PartialEq)]
pub struct FieldSpec {
    /// Its name: lowercase letters, digits and underscores, starting with a
    /// letter, e.g. `joint_positions`.
    pub name: String,
    /// The type of each value.
    pub ty: ScalarType,
    /// Byte offset of the first value in the payload.
    pub offset: usize,
    /// Number of values, one after another: 1 for a single value.
    pub count: usize,
    /// Byte order of each value.
    pub endianness: Endianness,
    /// Multiplier applied to each value, e.g. `0.001` for a value sent in
    /// thousandths. Scaled values are floats. Not allowed on `bool`.
    pub scale: Option<f64>,
    /// The unit of the (scaled) value, e.g. `rad`.
    pub unit: Option<String>,
}

impl FieldSpec {
    /// A single little-endian value, unscaled and without a unit.
    #[must_use]
    pub fn new(name: &str, ty: ScalarType, offset: usize) -> Self {
        Self {
            name: name.to_owned(),
            ty,
            offset,
            count: 1,
            endianness: Endianness::Little,
            scale: None,
            unit: None,
        }
    }

    /// The byte just past the field's last value.
    #[must_use]
    pub fn end(&self) -> usize {
        self.offset
            .saturating_add(self.ty.size().saturating_mul(self.count))
    }
}

/// A fixed binary layout: the fields of every payload of a stream.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct StructLayout {
    /// The fields, in the order they are output.
    pub fields: Vec<FieldSpec>,
}

impl StructLayout {
    /// Checks the layout and compiles it into a decoder.
    ///
    /// # Errors
    ///
    /// Every problem found, each `decoder.invalid_layout` naming its field
    /// and its position in the layout (1 for the first field):
    /// no fields, an invalid or repeated name, an empty array, a scale that
    /// is 0, not finite or on a `bool`, an empty unit, or a field ending past
    /// the largest UDP message.
    pub fn compile(&self) -> Result<StructDecoder, Vec<KaptoError>> {
        let mut errors = Vec::new();
        if self.fields.is_empty() {
            errors.push(invalid("the layout has no fields".to_owned()));
        }
        let mut names = BTreeSet::new();
        for (position, field) in (1_usize..).zip(&self.fields) {
            for problem in problems(field, &mut names) {
                errors.push(
                    invalid(problem)
                        .context("field", field.name.clone())
                        .context("position", position.to_string()),
                );
            }
        }
        if !errors.is_empty() {
            return Err(errors);
        }

        let mut reads = Vec::new();
        for field in &self.fields {
            for element in 0..field.count {
                reads.push(Read {
                    offset: field.offset + element * field.ty.size(),
                    ty: field.ty,
                    endianness: field.endianness,
                    scale: field.scale,
                });
            }
        }
        Ok(StructDecoder {
            fields: self
                .fields
                .iter()
                .map(|field| FieldDescription {
                    name: field.name.clone(),
                    unit: field.unit.clone(),
                    count: field.count,
                    ty: field.ty,
                })
                .collect(),
            needed: self.fields.iter().map(FieldSpec::end).max().unwrap_or(0),
            reads: reads.into_boxed_slice(),
        })
    }
}

fn invalid(detail: String) -> KaptoError {
    KaptoError::new(ErrorCode::DecoderInvalidLayout).detail(detail)
}

/// Everything wrong with one field.
fn problems(field: &FieldSpec, names: &mut BTreeSet<String>) -> Vec<String> {
    let mut problems = Vec::new();
    let name = &field.name;
    if !is_name(name) {
        problems.push(format!(
            "`{name}` isn't a valid field name: use lowercase letters, digits and underscores, starting with a letter"
        ));
    } else if !names.insert(name.clone()) {
        problems.push(format!("`{name}` is declared more than once"));
    }
    if field.count == 0 {
        problems.push(format!("`{name}` is an array of 0 values"));
    }
    match field.scale {
        Some(_) if field.ty == ScalarType::Bool => {
            problems.push(format!("`{name}` is a bool, which can't be scaled"));
        }
        Some(scale) if !scale.is_finite() || scale == 0.0 => {
            problems.push(format!(
                "`{name}` has scale {scale}; it must be a finite number other than 0"
            ));
        }
        _ => {}
    }
    if field.unit.as_deref().is_some_and(str::is_empty) {
        problems.push(format!("`{name}` has an empty unit; leave it out instead"));
    }
    if field.end() > MAX_MESSAGE_BYTES {
        problems.push(format!(
            "`{name}` ends at byte {}, past the largest UDP message ({MAX_MESSAGE_BYTES} bytes)",
            field.end()
        ));
    }
    problems
}

/// One value to read: where, as what, and how to scale it.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Read {
    offset: usize,
    ty: ScalarType,
    endianness: Endianness,
    scale: Option<f64>,
}

/// A compiled [`StructLayout`].
#[derive(Clone, Debug, PartialEq)]
pub struct StructDecoder {
    fields: Vec<FieldDescription>,
    reads: Box<[Read]>,
    needed: usize,
}

impl StructDecoder {
    /// The shortest payload this decoder can read: bytes beyond it are
    /// ignored.
    #[must_use]
    pub fn min_payload_len(&self) -> usize {
        self.needed
    }
}

impl Decoder for StructDecoder {
    fn fields(&self) -> &[FieldDescription] {
        &self.fields
    }

    fn value_count(&self) -> usize {
        self.reads.len()
    }

    fn decode(&self, payload: &[u8], out: &mut [Value]) -> Result<(), DecodeError> {
        if out.len() != self.reads.len() {
            return Err(DecodeError::OutputSize {
                needed: self.reads.len(),
                got: out.len(),
            });
        }
        let too_short = DecodeError::TooShort {
            needed: self.needed,
            got: payload.len(),
        };
        if payload.len() < self.needed {
            return Err(too_short);
        }
        for (read, value) in self.reads.iter().zip(out.iter_mut()) {
            *value = read_value(payload, read).ok_or(too_short)?;
        }
        Ok(())
    }
}

/// `N` bytes at `offset`, or `None` if the payload ends first.
fn bytes<const N: usize>(payload: &[u8], offset: usize) -> Option<[u8; N]> {
    payload.get(offset..offset.checked_add(N)?)?.try_into().ok()
}

macro_rules! number {
    ($ty:ty, $payload:expr, $read:expr) => {{
        let raw = bytes::<{ size_of::<$ty>() }>($payload, $read.offset)?;
        match $read.endianness {
            Endianness::Little => <$ty>::from_le_bytes(raw),
            Endianness::Big => <$ty>::from_be_bytes(raw),
        }
    }};
}

#[allow(
    clippy::cast_precision_loss,
    reason = "a scaled integer becomes a float by definition"
)]
fn read_value(payload: &[u8], read: &Read) -> Option<Value> {
    let unsigned = |value: u64| match read.scale {
        Some(scale) => Value::Float(value as f64 * scale),
        None => Value::UInt(value),
    };
    let signed = |value: i64| match read.scale {
        Some(scale) => Value::Float(value as f64 * scale),
        None => Value::Int(value),
    };
    let float = |value: f64| Value::Float(read.scale.map_or(value, |scale| value * scale));
    Some(match read.ty {
        ScalarType::U8 => unsigned(number!(u8, payload, read).into()),
        ScalarType::U16 => unsigned(number!(u16, payload, read).into()),
        ScalarType::U32 => unsigned(number!(u32, payload, read).into()),
        ScalarType::U64 => unsigned(number!(u64, payload, read)),
        ScalarType::I8 => signed(number!(i8, payload, read).into()),
        ScalarType::I16 => signed(number!(i16, payload, read).into()),
        ScalarType::I32 => signed(number!(i32, payload, read).into()),
        ScalarType::I64 => signed(number!(i64, payload, read)),
        ScalarType::F32 => float(number!(f32, payload, read).into()),
        ScalarType::F64 => float(number!(f64, payload, read)),
        ScalarType::Bool => Value::Bool(bytes::<1>(payload, read.offset)?[0] != 0),
    })
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;

    /// The load generator's message: sequence number, sender clock, 7 joints.
    fn loadgen_layout() -> StructLayout {
        StructLayout {
            fields: vec![
                FieldSpec::new("sequence", ScalarType::U32, 0),
                FieldSpec {
                    unit: Some("ns".to_owned()),
                    ..FieldSpec::new("source_time", ScalarType::U64, 4)
                },
                FieldSpec {
                    count: 7,
                    unit: Some("rad".to_owned()),
                    ..FieldSpec::new("joint_positions", ScalarType::F64, 12)
                },
            ],
        }
    }

    fn decode(decoder: &StructDecoder, payload: &[u8]) -> Result<Vec<Value>, DecodeError> {
        let mut out = vec![Value::default(); decoder.value_count()];
        decoder.decode(payload, &mut out).map(|()| out)
    }

    #[test]
    fn the_load_generators_message_decodes_into_named_values() {
        let decoder = loadgen_layout().compile().unwrap();
        let mut payload = Vec::new();
        payload.extend(42_u32.to_le_bytes());
        payload.extend(1_772_700_703_000_000_000_u64.to_le_bytes());
        for joint in 0..7 {
            payload.extend((f64::from(joint) * 0.1).to_le_bytes());
        }
        let values = decode(&decoder, &payload).unwrap();
        assert_eq!(values[0], Value::UInt(42));
        assert_eq!(values[1], Value::UInt(1_772_700_703_000_000_000));
        assert_eq!(values[2], Value::Float(0.0));
        assert_eq!(values[8], Value::Float(6.0 * 0.1));
        let names: Vec<_> = decoder
            .fields()
            .iter()
            .map(|field| field.name.as_str())
            .collect();
        assert_eq!(names, ["sequence", "source_time", "joint_positions"]);
        assert_eq!(decoder.fields()[2].count, 7);
        assert_eq!(decoder.fields()[2].unit.as_deref(), Some("rad"));
        assert_eq!(decoder.min_payload_len(), 68);
    }

    #[test]
    fn big_endian_signed_and_bool_fields_decode() {
        let decoder = StructLayout {
            fields: vec![
                FieldSpec {
                    endianness: Endianness::Big,
                    ..FieldSpec::new("count", ScalarType::U16, 0)
                },
                FieldSpec::new("offset", ScalarType::I32, 2),
                FieldSpec::new("enabled", ScalarType::Bool, 6),
            ],
        }
        .compile()
        .unwrap();
        let mut payload = vec![0x01, 0x02];
        payload.extend((-5_i32).to_le_bytes());
        payload.push(7);
        assert_eq!(
            decode(&decoder, &payload).unwrap(),
            [Value::UInt(0x0102), Value::Int(-5), Value::Bool(true)]
        );
    }

    #[test]
    fn a_scale_turns_integers_into_floats_in_the_declared_unit() {
        let decoder = StructLayout {
            fields: vec![FieldSpec {
                scale: Some(0.001),
                unit: Some("m".to_owned()),
                ..FieldSpec::new("height", ScalarType::I16, 0)
            }],
        }
        .compile()
        .unwrap();
        let values = decode(&decoder, &(-1_500_i16).to_le_bytes()).unwrap();
        assert_eq!(values, [Value::Float(-1.5)]);
    }

    #[test]
    fn a_payload_too_short_for_the_layout_is_not_decoded() {
        let decoder = loadgen_layout().compile().unwrap();
        assert_eq!(
            decode(&decoder, &[0; 67]),
            Err(DecodeError::TooShort {
                needed: 68,
                got: 67
            })
        );
    }

    #[test]
    fn bytes_past_the_layout_are_ignored() {
        let decoder = StructLayout {
            fields: vec![FieldSpec::new("a", ScalarType::U8, 0)],
        }
        .compile()
        .unwrap();
        assert_eq!(decode(&decoder, &[9, 1, 2, 3]).unwrap(), [Value::UInt(9)]);
    }

    #[test]
    fn an_output_of_the_wrong_size_is_refused() {
        let decoder = loadgen_layout().compile().unwrap();
        let mut out = [Value::default(); 3];
        assert_eq!(
            decoder.decode(&[0; 68], &mut out),
            Err(DecodeError::OutputSize { needed: 9, got: 3 })
        );
    }

    #[test]
    fn every_layout_problem_is_reported_with_its_field() {
        let layout = StructLayout {
            fields: vec![
                FieldSpec::new("Bad-Name", ScalarType::U8, 0),
                FieldSpec::new("twice", ScalarType::U8, 1),
                FieldSpec::new("twice", ScalarType::U8, 2),
                FieldSpec {
                    count: 0,
                    ..FieldSpec::new("empty", ScalarType::U8, 3)
                },
                FieldSpec {
                    scale: Some(2.0),
                    ..FieldSpec::new("flag", ScalarType::Bool, 4)
                },
                FieldSpec {
                    scale: Some(0.0),
                    ..FieldSpec::new("zero_scale", ScalarType::U8, 5)
                },
                FieldSpec {
                    unit: Some(String::new()),
                    ..FieldSpec::new("no_unit", ScalarType::U8, 6)
                },
                FieldSpec::new("too_far", ScalarType::U64, 65_500),
            ],
        };
        let errors = layout.compile().unwrap_err();
        let messages: Vec<String> = errors.iter().map(KaptoError::message).collect();
        assert_eq!(errors.len(), 7, "{messages:#?}");
        assert!(
            errors
                .iter()
                .all(|error| error.code() == ErrorCode::DecoderInvalidLayout)
        );
        for expected in [
            "`Bad-Name` isn't a valid field name",
            "`twice` is declared more than once",
            "`empty` is an array of 0 values",
            "`flag` is a bool, which can't be scaled",
            "`zero_scale` has scale 0",
            "`no_unit` has an empty unit",
            "`too_far` ends at byte 65508",
        ] {
            assert!(
                messages.iter().any(|message| message.contains(expected)),
                "{expected}"
            );
        }
    }

    #[test]
    fn an_empty_layout_is_refused() {
        let errors = StructLayout::default().compile().unwrap_err();
        assert!(errors[0].message().contains("the layout has no fields"));
    }

    /// One field and the values written for it.
    #[derive(Clone, Debug)]
    struct Generated {
        ty: ScalarType,
        endianness: Endianness,
        raw: Vec<u64>,
    }

    fn generated() -> impl Strategy<Value = Generated> {
        (
            prop::sample::select(ScalarType::ALL.to_vec()),
            prop::bool::ANY,
            prop::collection::vec(any::<u64>(), 1..5),
        )
            .prop_map(|(ty, big, raw)| Generated {
                ty,
                endianness: if big {
                    Endianness::Big
                } else {
                    Endianness::Little
                },
                raw,
            })
    }

    /// Writes one value as the layout expects, independently of the decoder.
    fn write(ty: ScalarType, endianness: Endianness, raw: u64, out: &mut Vec<u8>) {
        let size = ty.size();
        let raw = if ty == ScalarType::Bool { raw & 1 } else { raw };
        let little = raw.to_le_bytes();
        match endianness {
            Endianness::Little => out.extend(&little[..size]),
            Endianness::Big => out.extend(little[..size].iter().rev()),
        }
    }

    /// What the decoder should produce for a raw value, worked out from the
    /// bit pattern rather than by decoding.
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_possible_wrap,
        reason = "reinterpreting bit patterns on purpose"
    )]
    fn expected(ty: ScalarType, raw: u64) -> Value {
        match ty {
            ScalarType::U8 => Value::UInt(raw & 0xff),
            ScalarType::U16 => Value::UInt(raw & 0xffff),
            ScalarType::U32 => Value::UInt(raw & 0xffff_ffff),
            ScalarType::U64 => Value::UInt(raw),
            ScalarType::I8 => Value::Int(i64::from(raw as u8 as i8)),
            ScalarType::I16 => Value::Int(i64::from(raw as u16 as i16)),
            ScalarType::I32 => Value::Int(i64::from(raw as u32 as i32)),
            ScalarType::I64 => Value::Int(raw as i64),
            ScalarType::F32 => Value::Float(f64::from(f32::from_bits(raw as u32))),
            ScalarType::F64 => Value::Float(f64::from_bits(raw)),
            ScalarType::Bool => Value::Bool(raw & 1 == 1),
        }
    }

    fn same(a: Value, b: Value) -> bool {
        match (a, b) {
            (Value::Float(a), Value::Float(b)) => a.to_bits() == b.to_bits(),
            _ => a == b,
        }
    }

    proptest! {
        #[test]
        fn any_packed_layout_decodes_what_was_written(fields in prop::collection::vec(generated(), 1..8)) {
            let mut layout = StructLayout::default();
            let mut payload = Vec::new();
            let mut wanted = Vec::new();
            for (index, field) in fields.iter().enumerate() {
                layout.fields.push(FieldSpec {
                    count: field.raw.len(),
                    endianness: field.endianness,
                    ..FieldSpec::new(&format!("field_{index}"), field.ty, payload.len())
                });
                for &raw in &field.raw {
                    write(field.ty, field.endianness, raw, &mut payload);
                    wanted.push(expected(field.ty, raw));
                }
            }
            let decoder = layout.compile().unwrap();
            let values = decode(&decoder, &payload).unwrap();
            prop_assert_eq!(values.len(), wanted.len());
            for (got, want) in values.into_iter().zip(wanted) {
                prop_assert!(same(got, want), "{got:?} != {want:?}");
            }
        }

        #[test]
        fn any_bytes_decode_or_fail_cleanly(payload in prop::collection::vec(any::<u8>(), 0..100)) {
            let decoder = loadgen_layout().compile().unwrap();
            let result = decode(&decoder, &payload);
            prop_assert_eq!(result.is_ok(), payload.len() >= 68);
        }
    }
}
