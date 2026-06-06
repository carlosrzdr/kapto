import { afterEach, describe, expect, it, vi } from "vitest";

import {
  ApiFailure,
  checkVersion,
  getEpisodes,
  getReplayMesh,
  getReplayScene,
  getReplayWindow,
  PREFLIGHT_TIMEOUT_MS,
  REQUEST_TIMEOUT_MS,
  runPreflight,
  startEpisode,
  watchEvents,
} from "./api";
import { API_VERSION } from "./api-types";

function answer(status: number, body: unknown) {
  const fetch = vi.fn().mockResolvedValue(new Response(JSON.stringify(body), { status }));
  vi.stubGlobal("fetch", fetch);
  return fetch;
}

afterEach(() => {
  vi.unstubAllGlobals();
});

describe("api", () => {
  it("accepts the API version it was built for", async () => {
    answer(200, { kapto: "0.0.0", api: API_VERSION });
    await expect(checkVersion()).resolves.toEqual({ kapto: "0.0.0", api: API_VERSION });
  });

  it("asks for a stretch of a replay in whole milliseconds outward, so its ends are never cut short", async () => {
    const fetch = answer(200, {});
    await getReplayWindow("20260310T120000.000Z-00000001", 1_773_566_914_000.4, 1_773_566_914_999.2, 800.4);
    const url = String(fetch.mock.calls[0]?.[0]);
    expect(url).toBe(
      "/api/episodes/20260310T120000.000Z-00000001/replay/window?from=1773566914000000000&to=1773566915000000000&points=800",
    );
  });

  it("gives a replay's refusal with its cause code", async () => {
    answer(404, { code: "viewer.replay_not_ready", message: "The replay isn't made", remedy: "Ask for it." });
    const failure = await getReplayWindow("20260310T120000.000Z-00000001", 0, 1, 1).catch((error: unknown) => error);
    expect((failure as ApiFailure).code).toBe("viewer.replay_not_ready");
  });

  it("asks for a replay's robot and its meshes by the episode, and says when it was recorded without one", async () => {
    const fetch = answer(200, {});
    await getReplayMesh("20260310T120000.000Z-00000001", 3);
    expect(String(fetch.mock.calls[0]?.[0])).toBe("/api/episodes/20260310T120000.000Z-00000001/replay/meshes/3");
    answer(404, { code: "viewer.replay_without_robot", message: "The replay shows no robot", remedy: "Record with a package." });
    const failure = await getReplayScene("20260310T120000.000Z-00000001").catch((error: unknown) => error);
    expect((failure as ApiFailure).code).toBe("viewer.replay_without_robot");
  });

  it("refuses another API version with a cause and a remedy", async () => {
    answer(200, { kapto: "9.0.0", api: API_VERSION + 1 });
    const failure = await checkVersion().catch((error: unknown) => error);
    expect(failure).toBeInstanceOf(ApiFailure);
    expect((failure as ApiFailure).code).toBe("ui.api_version_mismatch");
    expect((failure as ApiFailure).remedy).toMatch(/Reload/);
  });

  it("gives up on a request that isn't answered in time, with its cause", async () => {
    vi.useFakeTimers();
    // Never answers; only gives up when the request is aborted.
    const fetch = vi.fn(
      (_path: string, init: RequestInit) =>
        new Promise((_resolve, reject) => {
          init.signal?.addEventListener("abort", () => reject(new DOMException("aborted", "AbortError")));
        }),
    );
    vi.stubGlobal("fetch", fetch);
    const pending = startEpisode().catch((error: unknown) => error);
    await vi.advanceTimersByTimeAsync(REQUEST_TIMEOUT_MS - 1);
    expect(fetch.mock.calls[0]?.[1].signal?.aborted).toBe(false);
    await vi.advanceTimersByTimeAsync(1);
    const failure = await pending;
    expect((failure as ApiFailure).code).toBe("ui.server_slow");
    vi.useRealTimers();
  });

  it("sends changes as JSON and passes the API's refusal on", async () => {
    const fetch = answer(409, {
      code: "capture.already_recording",
      message: "An episode is already being recorded",
      remedy: "Stop the current episode first.",
    });
    const failure = await startEpisode().catch((error: unknown) => error);
    expect((failure as ApiFailure).code).toBe("capture.already_recording");
    const [path, init] = fetch.mock.calls[0] as [string, RequestInit];
    expect(path).toBe("/api/episodes/start");
    expect(init.method).toBe("POST");
    expect(init.headers).toEqual({ "Content-Type": "application/json" });
    expect(init.body).toBe('{"profile":null,"task":null}');
  });

  it("waives the failed pre-flight checks it is given, and only then says so", async () => {
    const fetch = answer(201, { episode: "e" });
    await startEpisode({ profile: "lab_ana", task: null }, ["preflight.stream_silent"]);
    await startEpisode({ profile: null, task: null }, []);
    const bodies = fetch.mock.calls.map((call) => (call[1] as RequestInit).body);
    expect(bodies).toEqual([
      '{"profile":"lab_ana","task":null,"waive":["preflight.stream_silent"]}',
      '{"profile":null,"task":null}',
    ]);
  });

  it("waits for pre-flight as long as it watches the streams, longer than for other requests", async () => {
    vi.useFakeTimers();
    const fetch = vi.fn(
      (_path: string, init: RequestInit) =>
        new Promise((_resolve, reject) => {
          init.signal?.addEventListener("abort", () => reject(new DOMException("aborted", "AbortError")));
        }),
    );
    vi.stubGlobal("fetch", fetch);
    const pending = runPreflight().catch((error: unknown) => error);
    await vi.advanceTimersByTimeAsync(REQUEST_TIMEOUT_MS);
    expect(fetch.mock.calls[0]?.[1].signal?.aborted).toBe(false);
    await vi.advanceTimersByTimeAsync(PREFLIGHT_TIMEOUT_MS - REQUEST_TIMEOUT_MS);
    const failure = (await pending) as ApiFailure;
    expect(failure.code).toBe("ui.server_slow");
    expect(failure.message).toContain("/api/preflight within 60 s");
    expect(fetch.mock.calls[0]?.[0]).toBe("/api/preflight");
    vi.useRealTimers();
  });

  it("asks for the episodes the quality checks accepted or rejected, or all of them", async () => {
    const fetch = answer(200, []);
    await getEpisodes(20, "rejected");
    await getEpisodes(5, "accepted");
    await getEpisodes();
    const paths = fetch.mock.calls.map((call) => call[0]);
    expect(paths).toEqual([
      "/api/episodes?limit=20&accepted=false",
      "/api/episodes?limit=5&accepted=true",
      "/api/episodes?limit=20",
    ]);
  });

  it("reports an unreachable server as such", async () => {
    vi.stubGlobal("fetch", vi.fn().mockRejectedValue(new TypeError("Failed to fetch")));
    const failure = await checkVersion().catch((error: unknown) => error);
    expect((failure as ApiFailure).code).toBe("ui.server_unreachable");
  });

  it("passes on each pushed event and opens the stream again when it closes", () => {
    vi.useFakeTimers();
    const sockets: FakeSocket[] = [];
    class FakeSocket {
      onmessage: ((message: { data: string }) => void) | null = null;
      onclose: (() => void) | null = null;
      closed = false;
      constructor(readonly url: URL) {
        sockets.push(this);
      }
      close() {
        this.closed = true;
      }
    }
    vi.stubGlobal("WebSocket", FakeSocket);
    const events: unknown[] = [];
    const stop = watchEvents((event) => events.push(event));

    expect(sockets[0]?.url.pathname).toBe("/api/events");
    expect(sockets[0]?.url.protocol).toBe("ws:");
    sockets[0]?.onmessage?.({ data: '{"event":"episode_ended","episode":"e1","finalised":true}' });
    sockets[0]?.onmessage?.({ data: "not json" });
    expect(events).toEqual([{ event: "episode_ended", episode: "e1", finalised: true }]);

    sockets[0]?.onclose?.();
    vi.advanceTimersByTime(2000);
    expect(sockets).toHaveLength(2);

    stop();
    expect(sockets[1]?.closed).toBe(true);
    sockets[1]?.onclose?.();
    vi.advanceTimersByTime(10_000);
    expect(sockets).toHaveLength(2);
    vi.useRealTimers();
  });
});
