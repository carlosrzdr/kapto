// SPDX-License-Identifier: Apache-2.0

//! The Kapto daemon: process supervisor, local REST and WebSocket API, and the embedded web UI.

#[cfg(unix)]
pub mod api;
pub mod cameras;
#[cfg(unix)]
pub mod episode;
pub mod episode_events;
#[cfg(unix)]
pub mod index;
#[cfg(unix)]
pub mod jobs;
pub mod layouts;
pub mod preflight;
#[cfg(unix)]
pub mod processes;
pub mod quality;
#[cfg(unix)]
pub mod replays;
pub mod retention;
#[cfg(unix)]
pub mod robot;
/// The robot as the web UI draws it, which an episode's replay also
/// carries.
pub use kapto_viz::page_scene as scene;
#[cfg(unix)]
pub mod sessions;
#[cfg(unix)]
pub mod settings;
#[cfg(unix)]
pub mod storage;
#[cfg(unix)]
pub mod supervisor;
#[cfg(unix)]
pub mod ui;
#[cfg(unix)]
pub mod vocabulary;

#[cfg(unix)]
pub use processes::{ProcessState, ProcessStatus, Processes};
#[cfg(unix)]
pub use supervisor::{ChildSpec, Event, Listener, RestartPolicy, Supervisor};
