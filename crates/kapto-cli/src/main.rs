// SPDX-License-Identifier: Apache-2.0

//! The `kapto` binary.

use std::process::ExitCode;

fn main() -> ExitCode {
    kapto_cli::run()
}
