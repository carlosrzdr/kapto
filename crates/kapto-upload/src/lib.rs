// SPDX-License-Identifier: Apache-2.0

//! Uploads to user-configured storage: the upload engine, storing through `object_store`.
//!
//! The engine talks to storage through `object_store`'s own interface,
//! [`ObjectStore`](object_store::ObjectStore) and, for large files put in
//! parts, [`MultipartStore`](object_store::multipart::MultipartStore).
//! `conformance::check` is what a store must do for the engine, and
//! `faulty::Faulty` makes a store fail as a test says; both are built for
//! this crate's tests and with the `testing` feature.

pub mod backoff;
#[cfg(any(test, feature = "testing"))]
pub mod conformance;
pub mod credentials;
pub mod episode;
#[cfg(any(test, feature = "testing"))]
pub mod faulty;
pub mod key;
#[cfg(test)]
mod network_cut;
pub mod pace;
pub mod probe;
#[cfg(any(test, feature = "testing"))]
pub mod rustfs;
pub mod stores;
pub mod throttle;
