// SPDX-License-Identifier: Apache-2.0

//! What the plots show: each stream's decoded values, by name, and which
//! are plotted. Fields that only ever climb, such as a sequence number or
//! the robot's own clock, aren't: they say nothing about the robot.

use kapto_config::StreamDeclaration;

/// One stream's decoded fields, as the live plots name them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StreamFields {
    /// The stream's ID.
    pub stream: String,
    /// Each field's name and how many values it holds, in decoding order.
    pub fields: Vec<(String, usize)>,
    /// The fields not plotted, by name.
    pub not_plotted: Vec<String>,
}

impl StreamFields {
    /// Each stream's decoded fields, to plot, but for its sequence number
    /// and its clocks (fields in units of time), which only ever climb and
    /// say nothing about the robot. A stream declared without a decoder is
    /// recorded only and has none. Layouts were already checked when
    /// `streams.toml` was loaded, so one that doesn't compile can't reach
    /// here; if it did, its stream would have no plots rather than stop
    /// the viewer.
    #[must_use]
    pub fn of(streams: &[StreamDeclaration]) -> Vec<Self> {
        streams
            .iter()
            .map(|stream| {
                let decoded = stream
                    .decoder
                    .as_ref()
                    .and_then(|decoder| decoder.compile().ok())
                    .map(|decoder| decoder.fields().to_vec())
                    .unwrap_or_default();
                Self {
                    stream: stream.id.to_string(),
                    fields: decoded
                        .iter()
                        .map(|field| (field.name.clone(), field.count))
                        .collect(),
                    not_plotted: stream.unplotted_fields(),
                }
            })
            .collect()
    }
}

/// Each decoded value of every stream, by name, and whether it is plotted.
#[derive(Debug)]
pub struct Plots {
    streams: Vec<StreamPlots>,
}

/// One stream's values.
#[derive(Debug)]
struct StreamPlots {
    /// The stream's ID.
    stream: String,
    /// Each decoded value's name and whether it is plotted.
    values: Vec<(String, bool)>,
}

impl Plots {
    /// Every value of `streams`, in stream order: a field holding one value
    /// as its name, each value of an array as `name[index]`.
    #[must_use]
    pub fn new(streams: &[StreamFields]) -> Self {
        let streams = streams
            .iter()
            .map(|stream| StreamPlots {
                stream: stream.stream.clone(),
                values: stream
                    .fields
                    .iter()
                    .flat_map(|(name, count)| {
                        let plotted = !stream.not_plotted.contains(name);
                        (0..*count).map(move |index| {
                            let named = if *count == 1 {
                                name.clone()
                            } else {
                                format!("{name}[{index}]")
                            };
                            (named, plotted)
                        })
                    })
                    .collect(),
            })
            .collect();
        Self { streams }
    }

    /// Each stream's ID, in declaration order, with each of its decoded
    /// values' names in decoding order (an array's as `name[index]`) and
    /// whether it is plotted.
    #[must_use]
    pub fn values(&self) -> Vec<(&str, Vec<(&str, bool)>)> {
        self.streams
            .iter()
            .map(|stream| {
                let values = stream
                    .values
                    .iter()
                    .map(|(name, plotted)| (name.as_str(), *plotted))
                    .collect();
                (stream.stream.as_str(), values)
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use kapto_config::{ConfigFile, parse_streams};

    use super::*;

    #[test]
    fn the_sample_arms_sequence_number_and_clock_are_not_plotted_and_the_rest_is() {
        let path =
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../packages/arm7/streams.toml");
        let streams = parse_streams(&ConfigFile {
            contents: std::fs::read_to_string(&path).unwrap(),
            path,
        })
        .unwrap();
        let [arm] = StreamFields::of(&streams).try_into().unwrap();
        assert_eq!(arm.not_plotted, ["sequence", "source_time"]);
        let names: Vec<&str> = arm.fields.iter().map(|(name, _)| name.as_str()).collect();
        assert_eq!(
            names,
            ["sequence", "source_time", "joint_positions", "gripper"]
        );
    }
}
