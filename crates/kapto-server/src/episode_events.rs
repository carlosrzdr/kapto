// SPDX-License-Identifier: Apache-2.0

//! What happened in an episode, as the API shows it: its events as the
//! worker read them from its recording, and the stretches it began but
//! never ended.

use std::path::Path;

use kapto_core::KaptoError;
use kapto_store::{Reader, unended};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::episode::stored_here;
use crate::vocabulary::SentEvent;

/// What happened in an episode.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct EpisodeEvents {
    /// Whether its events were read from its recording, which the worker
    /// does after it is recorded; until then there are none.
    pub read: bool,
    /// Every event, in the order the recording holds them.
    pub events: Vec<SentEvent>,
    /// Each beginning no later end of the same kind and ID closes: a
    /// stretch still open when the recording ended, e.g. an intervention
    /// whose end was never marked.
    pub unended: Vec<SentEvent>,
}

/// The events of finalised episode `id` in `data_dir`, or deleted here, as
/// `reader` has them.
///
/// # Errors
///
/// `api.invalid_request` if `id` isn't an episode ID;
/// `api.episode_not_found` if no episode has it, finalised or deleted
/// here; `store.query_failed` if they can't be read.
pub fn read(data_dir: &Path, reader: &Reader, id: &str) -> Result<EpisodeEvents, KaptoError> {
    stored_here(data_dir, reader, id)?;
    Ok(match reader.events(id)? {
        Some(events) => EpisodeEvents {
            read: true,
            unended: unended(&events)
                .into_iter()
                .cloned()
                .map(SentEvent::from)
                .collect(),
            events: events.into_iter().map(SentEvent::from).collect(),
        },
        None => EpisodeEvents {
            read: false,
            events: Vec::new(),
            unended: Vec::new(),
        },
    })
}
