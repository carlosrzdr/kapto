import { describe, expect, it } from "vitest";

import { type Mark, MarkLog } from "./marks";

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

  it("shows an end whose beginning it never heard as a moment, and forgets everything when cleared", () => {
    const log = new MarkLog();
    log.add([noted({ kind: "grasp", at_utc_ns: 6_000_000_000, phase: "end", id: "g0" })]);
    expect(log.items.map((item) => [item.label, item.endMs])).toEqual([["end of grasp", 6000]]);
    log.clear();
    expect(log.items).toEqual([]);
  });
});
