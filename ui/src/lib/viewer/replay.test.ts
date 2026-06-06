import { describe, expect, it } from "vitest";

import type { EpisodeEvents, EpisodeQuality } from "../api-types";
import { openingOf, replayItems } from "./replay";
import { plotStreams } from "./plots";

const second = (n: number) => 1_773_566_914_000_000_000 + n * 1e9;

describe("replayItems", () => {
  it("draws an episode's events as the live view does, and each moment a check found, in time order", () => {
    const events: EpisodeEvents = {
      read: true,
      unended: [],
      events: [
        { kind: "intervention", phase: "begin", id: "i1", at_utc_ns: second(2), source: "cli", attributes: {} },
        { kind: "note", phase: "moment", at_utc_ns: second(5), source: "page", text: "dropped it", attributes: {} },
        { kind: "intervention", phase: "end", id: "i1", at_utc_ns: second(8), source: "cli", attributes: {} },
      ],
    };
    const quality = {
      accepted: true,
      checks: [
        {
          check: "motion.frozen",
          kind: "advisory",
          message: "stopped dead",
          ranges: [{ start_utc_ns: second(3), end_utc_ns: second(4) }],
          values: {},
          verdict: "failed",
          version: 1,
        },
      ],
    } as unknown as EpisodeQuality;
    const items = replayItems(events, quality);
    expect(items.map((item) => [item.kind, item.startMs, item.endMs])).toEqual([
      ["intervention", second(2) / 1e6, second(8) / 1e6],
      ["check", second(3) / 1e6, second(4) / 1e6],
      ["note", second(5) / 1e6, second(5) / 1e6],
    ]);
    expect(items[1]?.label).toBe("motion.frozen: stopped dead");
    expect(items[2]?.label).toContain("dropped it");
  });

  it("draws nothing for an episode whose events and checks aren't known", () => {
    expect(replayItems(null, null)).toEqual([]);
  });
});

describe("openingOf", () => {
  it("names a replay's values as the live stream's opening does, so the live view plots them", () => {
    const opening = openingOf({
      start_utc_ns: second(0),
      end_utc_ns: second(9),
      links: ["base_link"],
      streams: [
        {
          name: "arm_joints",
          values: [
            { name: "sequence", plotted: false },
            { name: "joint_positions[0]", plotted: true },
          ],
        },
      ],
    });
    const plots = plotStreams(opening);
    expect(plots.map((stream) => stream.name)).toEqual(["arm_joints"]);
  });
});
