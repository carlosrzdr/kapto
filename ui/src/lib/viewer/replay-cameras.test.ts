import { describe, expect, it } from "vitest";

import { frameAt, replayCamera, steppedTo } from "./replay-cameras";

const camera = replayCamera({
  camera: "wrist_camera",
  times_utc_ns: [1_000_000_000, 1_033_000_000, 1_066_000_000, 1_100_000_000],
});

describe("a replayed camera", () => {
  it("has its frame times in milliseconds", () => {
    expect(camera).toEqual({ name: "wrist_camera", timesMs: [1000, 1033, 1066, 1100] });
  });

  it("shows the newest frame received at or before a moment, and none before its first", () => {
    expect(frameAt(camera, 999)).toBe(-1);
    expect(frameAt(camera, 1000)).toBe(0);
    expect(frameAt(camera, 1065.9)).toBe(1);
    expect(frameAt(camera, 1066)).toBe(2);
    expect(frameAt(camera, 5000)).toBe(3);
    expect(frameAt({ name: "none", timesMs: [] }, 1000)).toBe(-1);
  });

  it("steps to every frame in turn, and no further than its first and last", () => {
    expect(steppedTo(camera, 1040, 1)).toBe(1066);
    expect(steppedTo(camera, 1040, -1)).toBe(1000);
    expect(steppedTo(camera, 1000, -1)).toBeNull();
    expect(steppedTo(camera, 1100, 1)).toBeNull();
    // Before its first frame, the next is its first.
    expect(steppedTo(camera, 900, 1)).toBe(1000);
  });
});
