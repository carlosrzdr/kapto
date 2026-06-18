// SPDX-License-Identifier: Apache-2.0

//! With `ros2`, compiles the C++ that tears an rcl node down where the
//! middleware's exceptions can't reach Rust (`src/ros2/teardown.cpp`). It
//! includes no ROS 2 headers; rcl itself is linked through `r2r_rcl`.

fn main() {
    println!("cargo::rerun-if-changed=src/ros2/teardown.cpp");
    #[cfg(feature = "ros2")]
    cc::Build::new()
        .cpp(true)
        .std("c++17")
        .file("src/ros2/teardown.cpp")
        .warnings(true)
        .compile("kapto_rcl_teardown");
}
