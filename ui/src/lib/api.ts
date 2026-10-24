// Calls to Kapto's API, typed with the types generated from its OpenAPI
// document.

import {
  API_VERSION,
  type ApiError,
  type BinnedEpisode,
  type CameraFrames,
  type EmptiedBin,
  type EpisodeDetail,
  type EpisodeEvents,
  type EpisodeQuality,
  type EpisodeStatistics,
  type EpisodeSummary,
  type EpisodeUploads,
  type Event,
  type EventSent,
  type EventVocabulary,
  type PreflightRun,
  type ProcessStatus,
  type ProfilesOverview,
  type ReplayOpening,
  type ReplayStatus,
  type LayoutInput,
  type RobotOverview,
  type ViewLayout,
  type RobotScene,
  type SendEvent,
  type SettingsOverview,
  type DestinationTested,
  type Started,
  type Status,
  type Storage,
  type Stopped,
  type TasksOverview,
  type UploadsOverview,
  type UploadsRetried,
  type Version,
} from "./api-types";

/** A request the API refused or couldn't answer, with its cause. */
export class ApiFailure extends Error {
  readonly code: string;
  readonly remedy: string;

  constructor(error: ApiError) {
    super(error.message);
    this.name = "ApiFailure";
    this.code = error.code;
    this.remedy = error.remedy;
  }
}

/**
 * How long a request may go unanswered before it fails. A request that
 * never ends would otherwise hold the page's place in the browser's few
 * connections to Kapto, and what waits on it, for good.
 */
export const REQUEST_TIMEOUT_MS = 15_000;

async function request<T>(path: string, init?: RequestInit, timeoutMs = REQUEST_TIMEOUT_MS): Promise<T> {
  const abort = new AbortController();
  const timer = setTimeout(() => abort.abort(), timeoutMs);
  let response: Response;
  let body: unknown;
  try {
    response = await fetch(path, { ...init, signal: abort.signal });
    body = await response.json().catch(() => null);
  } catch (error) {
    throw new ApiFailure(
      abort.signal.aborted
        ? {
            code: "ui.server_slow",
            message: `Kapto didn't answer ${path} within ${timeoutMs / 1000} s`,
            remedy: "Check what `kapto serve` is doing; the page asks again by itself.",
          }
        : {
            code: "ui.server_unreachable",
            message: `Kapto doesn't answer: ${String(error)}`,
            remedy: "Check that `kapto serve` is running, then reload the page.",
          },
    );
  } finally {
    clearTimeout(timer);
  }
  if (!response.ok) {
    throw new ApiFailure(
      isApiError(body)
        ? body
        : {
            code: "ui.unexpected_response",
            message: `${path} answered ${response.status}`,
            remedy: "Reload the page; if it happens again, report it.",
          },
    );
  }
  return body as T;
}

function isApiError(body: unknown): body is ApiError {
  return typeof body === "object" && body !== null && "code" in body && "message" in body;
}

/** A change, sent as JSON so the API knows it came from a page that may. */
function post<T>(path: string, body: object, timeoutMs = REQUEST_TIMEOUT_MS): Promise<T> {
  return request<T>(
    path,
    {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify(body),
    },
    timeoutMs,
  );
}

/** Checks that the API is the version this UI was built for. */
export async function checkVersion(): Promise<Version> {
  const version = await request<Version>("/api/version");
  if (version.api !== API_VERSION) {
    throw new ApiFailure({
      code: "ui.api_version_mismatch",
      message: `This page was built for API version ${API_VERSION}, but Kapto ${version.kapto} serves version ${version.api}.`,
      remedy: "Reload the page so the browser takes the UI that Kapto serves now.",
    });
  }
  return version;
}

export const getStatus = (): Promise<Status> => request<Status>("/api/status");

export const getProcesses = (): Promise<ProcessStatus[]> =>
  request<ProcessStatus[]>("/api/processes");

export const getRobot = (): Promise<RobotOverview> => request<RobotOverview>("/api/robot");

/** The robot as the page draws it: its links and their shapes. */
export const getRobotScene = (): Promise<RobotScene> => request<RobotScene>("/api/robot/scene");

/** The binary answer to `path`, failing with the API's error if it has one. */
async function binary(path: string): Promise<ArrayBuffer> {
  const abort = new AbortController();
  const timer = setTimeout(() => abort.abort(), REQUEST_TIMEOUT_MS);
  try {
    const response = await fetch(path, { signal: abort.signal });
    if (!response.ok) {
      const body: unknown = await response.json().catch(() => null);
      throw new ApiFailure(
        isApiError(body)
          ? body
          : { code: "ui.unexpected_response", message: `${path} answered ${response.status}`, remedy: "Reload the page." },
      );
    }
    return await response.arrayBuffer();
  } finally {
    clearTimeout(timer);
  }
}

/** One of the scene's meshes, in its binary form (see `viewer/mesh.ts`). */
export const getMesh = (n: number): Promise<ArrayBuffer> => binary(`/api/robot/meshes/${n}`);

export const getSettings = (): Promise<SettingsOverview> =>
  request<SettingsOverview>("/api/settings");

export const getStorage = (): Promise<Storage> => request<Storage>("/api/storage");

/** Which episodes to list by what the quality checks decided. */
export type QualityFilter = "all" | "accepted" | "rejected";

/**
 * The `limit` newest episodes, only those `filter` keeps and, if given,
 * with an event of kind `event`; those in the bin if `inBin`, and none of
 * them otherwise.
 */
export const getEpisodes = (
  limit = 20,
  filter: QualityFilter = "all",
  event: string | null = null,
  inBin = false,
): Promise<EpisodeSummary[]> =>
  request<EpisodeSummary[]>(
    `/api/episodes?limit=${limit}${filter === "all" ? "" : `&accepted=${filter === "accepted"}`}${
      event === null ? "" : `&event=${encodeURIComponent(event)}`
    }${inBin ? "&bin=true" : ""}`,
  );

/** Deletes an episode from the web UI: into the bin, until it is emptied or put back. */
export const deleteEpisode = (id: string): Promise<BinnedEpisode> =>
  request<BinnedEpisode>(`/api/episodes/${encodeURIComponent(id)}`, {
    method: "DELETE",
    headers: { "Kapto-Client": "web UI" },
  });

/** Puts an episode back from the bin. */
export const restoreEpisode = (id: string): Promise<unknown> =>
  post<unknown>(`/api/episodes/${encodeURIComponent(id)}/restore`, {});

/** Empties the bin: every episode in it is deleted for good as the worker gets to it. */
export const emptyBin = (): Promise<EmptiedBin> => post<EmptiedBin>("/api/bin/empty", {});

export const getEpisode = (id: string): Promise<EpisodeDetail> =>
  request<EpisodeDetail>(`/api/episodes/${encodeURIComponent(id)}`);

export const getEpisodeQuality = (id: string): Promise<EpisodeQuality> =>
  request<EpisodeQuality>(`/api/episodes/${encodeURIComponent(id)}/qa`);

export const getEpisodeEvents = (id: string): Promise<EpisodeEvents> =>
  request<EpisodeEvents>(`/api/episodes/${encodeURIComponent(id)}/events`);

export const getEpisodeStatistics = (id: string): Promise<EpisodeStatistics> =>
  request<EpisodeStatistics>(`/api/episodes/${encodeURIComponent(id)}/statistics`);

/** Every destination's counts, and every upload not done yet. */
export const getUploads = (): Promise<UploadsOverview> => request<UploadsOverview>("/api/uploads");

/** One episode's uploads: where each object went, and whether it is still here. */
export const getEpisodeUploads = (id: string): Promise<EpisodeUploads> =>
  request<EpisodeUploads>(`/api/episodes/${encodeURIComponent(id)}/uploads`);

/** Tries the episode's failed uploads again now. */
export const retryUploads = (id: string): Promise<UploadsRetried> =>
  post<UploadsRetried>(`/api/episodes/${encodeURIComponent(id)}/uploads/retry`, {});

/**
 * How long a destination's test may take: three requests to the store,
 * each of which may wait for it up to `upload.timeout_request`.
 */
export const TEST_TIMEOUT_MS = 60_000;

/** Writes, reads back and removes a probe object at the destination named. */
export const testDestination = (name: string): Promise<DestinationTested> =>
  post<DestinationTested>(`/api/uploads/destinations/${encodeURIComponent(name)}/test`, {}, TEST_TIMEOUT_MS);

/** Asks for an episode's replay; answers where it is. */
export const askReplay = (id: string): Promise<ReplayStatus> =>
  post<ReplayStatus>(`/api/episodes/${encodeURIComponent(id)}/replay`, {});

/** Where an episode's replay is, asking for nothing. */
export const getReplay = (id: string): Promise<ReplayStatus> =>
  request<ReplayStatus>(`/api/episodes/${encodeURIComponent(id)}/replay`);

/** What a made replay shows: its span, and the links and values its stretches give. */
export const getReplayOpening = (id: string): Promise<ReplayOpening> =>
  request<ReplayOpening>(`/api/episodes/${encodeURIComponent(id)}/replay/opening`);

/** The robot a made replay was recorded with, as the page draws it. */
export const getReplayScene = (id: string): Promise<RobotScene> =>
  request<RobotScene>(`/api/episodes/${encodeURIComponent(id)}/replay/scene`);

/** One of a made replay's robot's meshes, in its binary form (see `viewer/mesh.ts`). */
export const getReplayMesh = (id: string, n: number): Promise<ArrayBuffer> =>
  binary(`/api/episodes/${encodeURIComponent(id)}/replay/meshes/${n}`);

/**
 * A stretch of a made replay, from `fromMs` to `toMs` (UTC milliseconds),
 * merged into `points` parts, in the binary form `viewer/stream.ts` reads.
 */
/** When each frame of one of episode `id`'s cameras was received. */
export const getCameraFrames = (id: string, camera: string): Promise<CameraFrames> =>
  request<CameraFrames>(`/api/episodes/${encodeURIComponent(id)}/cameras/${encodeURIComponent(camera)}/frames`);

/** Frame `n` of one of episode `id`'s cameras, as a picture the browser decodes. */
export async function getCameraPicture(id: string, camera: string, n: number): Promise<Blob> {
  const path = `/api/episodes/${encodeURIComponent(id)}/cameras/${encodeURIComponent(camera)}/frames/${n}`;
  const bytes = await binary(path);
  return new Blob([bytes]);
}

export function getReplayWindow(id: string, fromMs: number, toMs: number, points: number): Promise<ArrayBuffer> {
  // Nanoseconds don't fit a JavaScript number exactly: written by hand,
  // whole milliseconds outward, so a stretch ending at the episode's first
  // message never ends just before it.
  const nanos = (ms: number) => (BigInt(ms) * 1_000_000n).toString();
  const [from, to] = [nanos(Math.floor(fromMs)), nanos(Math.ceil(toMs))];
  return binary(
    `/api/episodes/${encodeURIComponent(id)}/replay/window?from=${from}&to=${to}&points=${Math.max(1, Math.round(points))}`,
  );
}

export const getProfiles = (): Promise<ProfilesOverview> =>
  request<ProfilesOverview>("/api/profiles");

export const getTasks = (): Promise<TasksOverview> => request<TasksOverview>("/api/tasks");

/** What an episode is recorded with: a profile and a task, either may be none. */
export interface SessionChoice {
  profile: string | null;
  task: string | null;
}

/**
 * Starts an episode, recording despite the failed pre-flight checks whose
 * codes are in `waive`.
 */
export const startEpisode = (
  choice: SessionChoice = { profile: null, task: null },
  waive: string[] = [],
): Promise<Started> => post<Started>("/api/episodes/start", waive.length > 0 ? { ...choice, waive } : choice);

/**
 * How long pre-flight may take to answer: it watches the streams for
 * `capture.preflight_window` first, as long as Kapto waits for it.
 */
export const PREFLIGHT_TIMEOUT_MS = 60_000;

/** Runs pre-flight again; answers once it has ended. */
export const runPreflight = (): Promise<PreflightRun> =>
  post<PreflightRun>("/api/preflight", {}, PREFLIGHT_TIMEOUT_MS);

export const stopEpisode = (): Promise<Stopped> => post<Stopped>("/api/episodes/stop", {});

/** The kinds of events that may be sent, each with its name and colour. */
export const getEventKinds = (): Promise<EventVocabulary> => request<EventVocabulary>("/api/event-kinds");

/** Sends an event: recorded in the episode being recorded, if one is. */
export const sendEvent = (event: SendEvent): Promise<EventSent> => post<EventSent>("/api/episodes/events", event);

/** Saves `layout` as the robot package's default, for every browser: what `/api/robot` gives from then on. */
export const saveDefaultLayout = (layout: LayoutInput): Promise<ViewLayout> =>
  request<ViewLayout>("/api/layout", {
    method: "PUT",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify(layout),
  });

/** `layout` as the view draws it, each panel with what the robot's streams lack of it; nothing is saved. */
export const checkLayout = (layout: LayoutInput): Promise<ViewLayout> => post<ViewLayout>("/api/layout/check", layout);

/** Removes the robot package's default layout: its own is back. */
export const removeDefaultLayout = (): Promise<ViewLayout> => request<ViewLayout>("/api/layout", { method: "DELETE" });

/** How long to wait before opening the event stream again after it closed. */
const RECONNECT_MS = 2000;

/**
 * Calls `onevent` with each event the API pushes over `/api/events`,
 * opening the stream again whenever it closes, e.g. while Kapto restarts.
 * Returns a function that stops listening.
 */
export function watchEvents(onevent: (event: Event) => void): () => void {
  let socket: WebSocket | null = null;
  let retry: ReturnType<typeof setTimeout> | undefined;
  let stopped = false;
  const url = new URL("/api/events", window.location.href);
  url.protocol = url.protocol === "https:" ? "wss:" : "ws:";

  const open = () => {
    socket = new WebSocket(url);
    socket.onmessage = (message) => {
      try {
        onevent(JSON.parse(String(message.data)) as Event);
      } catch {
        // Not an event this page knows; the next one may be.
      }
    };
    socket.onclose = () => {
      if (!stopped) {
        retry = setTimeout(open, RECONNECT_MS);
      }
    };
  };
  open();
  return () => {
    stopped = true;
    clearTimeout(retry);
    socket?.close();
  };
}
