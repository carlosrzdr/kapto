// SPDX-License-Identifier: Apache-2.0

//! The API's handlers, called through its router, with a stand-in for the
//! capture process on a real control socket.

#![cfg(unix)]
#![allow(
    clippy::unwrap_used,
    clippy::panic,
    reason = "test helpers fail the test loudly when the setup itself breaks"
)]

mod access;
mod capture;
mod common;
mod episodes;
mod events;
mod profiles;
mod replay;
mod robot;
mod status;
mod uploads;
