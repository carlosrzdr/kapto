import { describe, expect, it } from "vitest";

import { SERIES } from "../theme";
import { LANE_WIDTH, laneMarks, type Mark, MarkLog, markColor, markTip } from "./marks";

const kapto = (kind: string, at: number, stream: string): Mark => ({
  noted: { event: { kind, at_utc_ns: at * 1e6, phase: "moment", source: "kapto", attributes: { stream } }, episode: null },
});

const noted = (event: Record<string, unknown>): Mark => ({
  noted: { event: { source: "api", ...event } as never, episode: "e1" },
});

describe("MarkLog", () => {
  it("makes moments, stretches begun and ended, whole stretches and episodes into items", () => {
    const log = new MarkLog();
    log.add([
      { episode_started: { episode: "e1", at_utc_ns: 1_000_000_000 } },
      noted({ kind: "note", at_utc_ns: 2_000_000_000, text: "look" }),
      noted({ kind: "grasp", at_utc_ns: 3_000_000_000, phase: "begin", id: "g1" }),
      noted({ kind: "slip", at_utc_ns: 4_000_000_000, until_utc_ns: 5_000_000_000, phase: "stretch" }),
    ]);
    expect(log.items.map((item) => [item.kind, item.startMs, item.endMs])).toEqual([
      ["episode", 1000, 1000],
      ["note", 2000, 2000],
      ["grasp", 3000, null],
      ["slip", 4000, 5000],
    ]);
    expect(log.items[1]?.label).toBe("note (api) · look");
    log.add([
      noted({ kind: "grasp", at_utc_ns: 6_000_000_000, phase: "end", id: "g1" }),
      { episode_ended: { episode: "e1", at_utc_ns: 7_000_000_000, finalised: false } },
    ]);
    expect(log.items[2]?.endMs).toBe(6000);
    expect(log.items.at(-1)?.label).toBe("Episode aborted");
  });

  it("knows when the episode being recorded started, until it ends or the log is cleared", () => {
    const log = new MarkLog();
    expect(log.recordingSinceMs).toBeNull();
    log.add([{ episode_started: { episode: "e1", at_utc_ns: 1_000_000_000 } }, noted({ kind: "note", at_utc_ns: 2_000_000_000 })]);
    expect(log.recordingSinceMs).toBe(1000);
    log.add([{ episode_ended: { episode: "e1", at_utc_ns: 3_000_000_000, finalised: true } }]);
    expect(log.recordingSinceMs).toBeNull();
    log.add([{ episode_started: { episode: "e2", at_utc_ns: 4_000_000_000 } }]);
    log.clear();
    expect(log.recordingSinceMs).toBeNull();
  });

  it("shows an end whose beginning it never heard as a moment, and forgets everything when cleared", () => {
    const log = new MarkLog();
    log.add([noted({ kind: "grasp", at_utc_ns: 6_000_000_000, phase: "end", id: "g0" })]);
    expect(log.items.map((item) => [item.label, item.endMs])).toEqual([["end of grasp", 6000]]);
    log.clear();
    expect(log.items).toEqual([]);
  });
});

describe("markTip", () => {
  const at = new Date(2026, 2, 10, 14, 46, 5, 250).getTime();

  it("says what a moment was and when, to the millisecond", () => {
    expect(markTip({ kind: "note", startMs: at, endMs: at, label: "note (page)" })).toEqual({
      what: "note (page)",
      when: "14:46:05.250",
      lasting: null,
    });
  });

  it("says how long a stretch lasted, or that it goes on", () => {
    const tip = (endMs: number | null) => markTip({ kind: "grasp", startMs: at, endMs, label: "grasp (api)" }).lasting;
    expect(tip(at + 450)).toBe("for 450\u00a0ms");
    expect(tip(at + 2_400)).toBe("for 2.4\u00a0s");
    expect(tip(at + 75_000)).toBe("for 1:15");
    expect(tip(null)).toBe("still going");
  });
});

describe("MarkLog's windows of Kapto's own", () => {
  it("draws a stream silent until it came back as one mark, each stream its own", () => {
    const log = new MarkLog();
    log.add([kapto("kapto.stream_silent", 2000, "left"), kapto("kapto.stream_silent", 2000, "right"), kapto("kapto.stream_back", 8000, "left")]);
    expect(log.items).toEqual([
      { kind: "kapto.stream_silent", startMs: 2000, endMs: 8000, label: "kapto.stream_silent (kapto) · left" },
      { kind: "kapto.stream_silent", startMs: 2000, endMs: null, label: "kapto.stream_silent (kapto) · right" },
    ]);
  });
});

describe("laneMarks", () => {
  const item = (kind: string, startMs: number, endMs: number | null) => ({ kind, startMs, endMs, label: kind });

  it("places each mark on the lane where the timeline's thumb stands at its moment, an open one up to the end", () => {
    const placed = laneMarks([item("note", 1_250, 1_250), item("grasp", 1_500, null), item("slip", 1_000, 1_100)], 1_000, 2_000, {}, 0);
    expect(placed.map(({ left, width }) => [left, width])).toEqual([
      [250, 4],
      [500, 500],
      [0, 100],
    ]);
  });

  it("leaves out what is outside the timeline, and keeps a moment at its end on the lane", () => {
    const placed = laneMarks([item("note", 500, 500), item("note", 2_500, 2_500), item("note", 2_000, 2_000)], 1_000, 2_000, {}, 0);
    expect(placed.map(({ left, width }) => [left, width])).toEqual([[LANE_WIDTH - 4, 4]]);
  });

  it("takes the pointer at least as wide as asked, centred on the mark and kept on the lane", () => {
    const [middle, first] = laneMarks([item("note", 1_500, 1_500), item("note", 1_000, 1_000)], 1_000, 2_000, {}, 40);
    expect([middle!.hitLeft, middle!.hit]).toEqual([482, 40]);
    expect([first!.hitLeft, first!.hit]).toEqual([0, 40]);
  });

  it("colours every mark, Kapto's own, checks and episodes too, from the series colours, never a state's", () => {
    const kinds = ["episode", "check", "kapto.stream_silent", "kapto.stream_back", "note", "grasp"];
    const placed = laneMarks(
      kinds.map((kind) => item(kind, 1_500, 1_500)),
      1_000,
      2_000,
      {},
      0,
    );
    // Green, sky blue, amber, red and the muted grey of `app.css`.
    const states = ["#22c55e", "#38bdf8", "#f59e0b", "#f87171", "#9ca3a8"];
    expect(placed).toHaveLength(kinds.length);
    for (const { color } of placed) {
      expect(SERIES).toContain(color);
      expect(states).not.toContain(color);
    }
  });

  it("takes a kind's configured colour, but never for a kind of the page's or Kapto's own", () => {
    const colors = { grasp: "#123456", check: "#654321", episode: "#654321", "kapto.stream_silent": "#654321" };
    expect(markColor("grasp", colors)).toBe("#123456");
    for (const own of ["check", "episode", "kapto.stream_silent"]) {
      expect(markColor(own, colors)).toBe(markColor(own, {}));
    }
  });
});
