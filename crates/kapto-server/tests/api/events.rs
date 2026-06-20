// SPDX-License-Identifier: Apache-2.0

//! The event stream: what capture announces, as it happens.

use axum::http::{StatusCode, header};
use kapto_capture::control::{Request, Response, Status};
use kapto_server::api::{ApiState, router};
use kapto_testkit::scratch;
use serde_json::{Value, json};

use crate::common::{FakeCapture, idle, recording, state};

/// Serves the API on a free loopback port until the test ends.
async fn serve(state: ApiState) -> u16 {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move { axum::serve(listener, router(state)).await });
    port
}

#[tokio::test]
async fn the_event_stream_tells_an_episode_started_then_the_status() {
    use futures_util::StreamExt;
    use tokio_tungstenite::tungstenite::Message;

    let folder = scratch("events");
    let _capture = FakeCapture::start(&folder.join("capture.sock"), recording);
    let port = serve(state(&folder)).await;
    let (mut socket, _) =
        tokio_tungstenite::connect_async(format!("ws://127.0.0.1:{port}/api/events"))
            .await
            .unwrap();
    let mut next = async || {
        let Some(Ok(Message::Text(text))) = socket.next().await else {
            panic!("no event");
        };
        serde_json::from_str::<Value>(&text).unwrap()
    };
    assert_eq!(
        next().await,
        json!({"event": "episode_started", "episode": "20260310T120000.000Z-1a2b3c4d"})
    );
    let status = next().await;
    assert_eq!(status["event"], "status");
    assert_eq!(status["status"]["capture"]["streams"][0]["written"], 13_990);
}

/// A capture process recording nothing, as its status says.
fn not_recording(request: &Request) -> Response {
    match request {
        Request::Status => Response::Status(Status::default()),
        other => idle(other),
    }
}

#[tokio::test]
async fn an_episode_capture_announces_is_told_though_polling_never_saw_it() {
    use futures_util::StreamExt;
    use kapto_capture::control::Announcement;
    use tokio_tungstenite::tungstenite::Message;

    let folder = scratch("events-announced");
    let _capture = FakeCapture::start(&folder.join("capture.sock"), not_recording);
    let state = state(&folder);
    let announcements = state.announcements.clone();
    let port = serve(state).await;
    let (mut socket, _) =
        tokio_tungstenite::connect_async(format!("ws://127.0.0.1:{port}/api/events"))
            .await
            .unwrap();
    let mut next = async || {
        let Some(Ok(Message::Text(text))) = socket.next().await else {
            panic!("no event");
        };
        serde_json::from_str::<Value>(&text).unwrap()
    };
    // Listening once the first status has come.
    assert_eq!(next().await["event"], "status");
    let episode = "20260310T120000.000Z-1a2b3c4d";
    announcements.announce(Announcement::EpisodeStarted {
        episode: episode.into(),
    });
    announcements.announce(Announcement::EpisodeEnded {
        episode: episode.into(),
        finalised: true,
    });
    assert_eq!(
        next().await,
        json!({"event": "episode_started", "episode": episode})
    );
    assert_eq!(
        next().await,
        json!({"event": "episode_ended", "episode": episode, "finalised": true})
    );
}

#[tokio::test]
async fn another_site_s_page_may_not_open_the_event_stream() {
    use tokio_tungstenite::tungstenite::Error;
    use tokio_tungstenite::tungstenite::client::IntoClientRequest;

    let folder = scratch("events-origin");
    let port = serve(state(&folder)).await;
    let mut request = format!("ws://127.0.0.1:{port}/api/events")
        .into_client_request()
        .unwrap();
    request
        .headers_mut()
        .insert(header::ORIGIN, "http://attacker.example".parse().unwrap());
    let Err(Error::Http(response)) = tokio_tungstenite::connect_async(request).await else {
        panic!("the connection was accepted");
    };
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    let body: Value = serde_json::from_slice(response.body().as_deref().unwrap()).unwrap();
    assert_eq!(body["code"], "api.origin_not_allowed");
}
