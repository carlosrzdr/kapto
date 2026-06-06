// SPDX-License-Identifier: Apache-2.0

//! Records a three-second episode through Kapto's API and prints what each
//! stream received. Anything Kapto refuses ends it with the cause code.

use std::time::Duration;

use reqwest::blocking::{Client, RequestBuilder};
use serde_json::{Value, json};

/// Where `kapto serve` answers.
const API: &str = "http://127.0.0.1:7700/api";

type Error = Box<dyn std::error::Error>;

fn main() -> Result<(), Error> {
    let episode = record(API, Duration::from_secs(3))?;
    println!("Recorded episode {episode}");
    Ok(())
}

/// Records one episode lasting `length` and returns its ID.
pub fn record(api: &str, length: Duration) -> Result<String, Error> {
    // Stopping answers once the episode is finalised, which takes a moment.
    let client = Client::builder().timeout(Duration::from_secs(30)).build()?;
    let get = |path: &str| answer(client.get(format!("{api}{path}")));
    let post = |path: &str| answer(client.post(format!("{api}{path}")).json(&json!({})));

    let version = get("/version")?;
    if version["api"] != 1 {
        return Err(format!("this program knows API version 1, not {}", version["api"]).into());
    }

    let started = post("/episodes/start")?;
    let episode = started["episode"].as_str().unwrap_or_default();
    println!("Recording {episode}");
    std::thread::sleep(length);

    let streams = get("/status")?["capture"]["streams"].take();
    for stream in streams.as_array().unwrap_or(&Vec::new()) {
        println!(
            "{}: {} received, {} written",
            stream["stream"].as_str().unwrap_or_default(),
            stream["received"],
            stream["written"]
        );
    }

    let stopped = post("/episodes/stop")?;
    Ok(stopped["episode"].as_str().unwrap_or_default().to_owned())
}

/// Sends a request: the answer's JSON, or an error with Kapto's cause code.
fn answer(request: RequestBuilder) -> Result<Value, Error> {
    let response = request.send()?;
    let succeeded = response.status().is_success();
    let body: Value = response.json()?;
    if succeeded {
        Ok(body)
    } else {
        Err(format!("{}: {} {}", body["code"], body["message"], body["remedy"]).into())
    }
}
