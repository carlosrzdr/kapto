import { describe, expect, it } from "vitest";

import type { UploadStatus } from "./api-types";
import { measure, timeLeft } from "./uploads";

const MiB = 1024 * 1024;
const s = 1e9;

function upload(sent: number, reported: number | null, state = "uploading"): UploadStatus {
  return {
    episode: "ep1",
    destination: "archive",
    state,
    key: "arm7/2026-03-25/ep1",
    bytes_sent: sent,
    bytes_total: 100 * MiB,
    throttled: 0,
    started_utc_ns: 0,
    reported_utc_ns: reported,
  };
}

describe("measure", () => {
  it("measures bytes per second between two reports, by the worker's own times", () => {
    const first = measure(new Map(), [upload(10 * MiB, 1 * s)]);
    expect(first.get("ep1/archive")?.rate).toBeNull();
    const second = measure(first, [upload(30 * MiB, 3 * s)]);
    expect(second.get("ep1/archive")?.rate).toBe(10 * MiB);
  });

  it("keeps the rate while nothing new is reported", () => {
    const measured = measure(measure(new Map(), [upload(10 * MiB, 1 * s)]), [upload(30 * MiB, 3 * s)]);
    expect(measure(measured, [upload(30 * MiB, 3 * s)]).get("ep1/archive")?.rate).toBe(10 * MiB);
  });

  it("starts again when the count goes back, and forgets uploads no longer going", () => {
    const measured = measure(new Map(), [upload(30 * MiB, 3 * s)]);
    expect(measure(measured, [upload(5 * MiB, 9 * s)]).get("ep1/archive")?.rate).toBeNull();
    expect(measure(measured, [upload(30 * MiB, 3 * s, "failed")]).size).toBe(0);
    expect(measure(measured, [upload(0, null)]).size).toBe(0);
  });
});

describe("timeLeft", () => {
  it("is what is left over the rate, and unknown while it doesn't move", () => {
    expect(timeLeft(upload(40 * MiB, 1), 2 * MiB)).toBe(30);
    expect(timeLeft(upload(40 * MiB, 1), 0)).toBeNull();
    expect(timeLeft(upload(40 * MiB, 1), null)).toBeNull();
  });
});
