// SPDX-License-Identifier: Apache-2.0

//! What an episode can be recorded with: the session profiles for the
//! robot package, the tasks, and the kinds of event that can be sent.

use std::sync::Arc;

use axum::Json;
use axum::extract::State;

use crate::sessions::{self, ProfilesOverview, TasksOverview};
use crate::vocabulary::{self, EventVocabulary};

use super::ApiState;

/// The profiles saved in the configuration folder, each marked available if
/// it records the robot package Kapto runs, and what is wrong with any that
/// can't be read. Read again on each request.
#[utoipa::path(get, path = "/api/profiles", responses((status = 200, body = ProfilesOverview)))]
pub(super) async fn profiles(State(state): State<Arc<ApiState>>) -> Json<ProfilesOverview> {
    let (folder, package) = (state.config_folder.clone(), state.package.clone());
    let read =
        tokio::task::spawn_blocking(move || sessions::profiles(&folder, package.as_deref())).await;
    Json(read.unwrap_or_else(|_| ProfilesOverview {
        package: state.package.clone(),
        profiles: Vec::new(),
        problems: Vec::new(),
    }))
}

/// The tasks saved in the configuration folder, and what is wrong with any
/// that can't be read. Read again on each request.
#[utoipa::path(get, path = "/api/tasks", responses((status = 200, body = TasksOverview)))]
pub(super) async fn tasks(State(state): State<Arc<ApiState>>) -> Json<TasksOverview> {
    let folder = state.config_folder.clone();
    let read = tokio::task::spawn_blocking(move || sessions::tasks(&folder)).await;
    Json(read.unwrap_or_else(|_| TasksOverview {
        tasks: Vec::new(),
        problems: Vec::new(),
    }))
}

/// The kinds of events that may be sent: the vocabulary file's, or the
/// defaults without it, then each task's subtasks, and what is wrong with
/// the file. Read again on each request.
#[utoipa::path(get, path = "/api/event-kinds", responses((status = 200, body = EventVocabulary)))]
pub(super) async fn event_kinds(State(state): State<Arc<ApiState>>) -> Json<EventVocabulary> {
    let folder = state.config_folder.clone();
    let read = tokio::task::spawn_blocking(move || vocabulary::vocabulary(&folder)).await;
    Json(read.unwrap_or_else(|_| EventVocabulary {
        kinds: Vec::new(),
        problems: Vec::new(),
    }))
}
