import { describe, expect, it } from "vitest";

import { LEAD_MS, openedAt, playedTo } from "./play";

describe("playedTo", () => {
  it("moves forward from where it started at the speed chosen", () => {
    const started = { fromMs: 1_000_000, wallMs: 50 };
    expect(playedTo(started, 50, 1)).toBe(1_000_000);
    expect(playedTo(started, 1_050, 1)).toBe(1_001_000);
    expect(playedTo(started, 1_050, 2)).toBe(1_002_000);
    expect(playedTo(started, 1_050, 0.5)).toBe(1_000_500);
  });

  it("never goes back if the clock reads earlier than the start", () => {
    expect(playedTo({ fromMs: 10, wallMs: 100 }, 90, 1)).toBe(10);
  });
});

describe("openedAt", () => {
  it("opens an event a little before it, within what is kept", () => {
    expect(openedAt(10_000, 0)).toBe(10_000 - LEAD_MS);
    expect(openedAt(10_000, 9_500)).toBe(9_500);
  });
});
