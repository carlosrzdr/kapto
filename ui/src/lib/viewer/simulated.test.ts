import { describe, expect, it } from "vitest";

import type { SimulatedRun } from "../api-types";
import { replayItems, SIMULATION_KIND } from "./replay";
import { axisSimulated, SimulatedTime, simulatedLabel } from "./simulated";

const S = 1e9;
/** When the episode's first message arrived, UTC nanoseconds. */
const T0 = 1_800_000_000 * S;
const T0_MS = T0 / 1e6;

/** A run whose segments are lists of `[receive s after T0, simulated s]`. */
function run(...segments: [number, number][][]): SimulatedRun {
  return {
    start_ns: 0,
    end_ns: 0,
    simulated_ns: 0,
    real_ns: 0,
    paused_ns: 0,
    ratio: null,
    pauses: [],
    resets: [],
    segments: segments.map((points) =>
      points.map(([receive, simulated]) => ({ receive_utc_ns: T0 + receive * S, simulated_ns: simulated * S })),
    ),
  };
}

describe("a replay in simulated time", () => {
  // 1 s as received before the clock, half real time for 2 s, paused 3 s,
  // half again for 2 s, reset to 0, real time for 1 s; 1 s after it.
  const time = new SimulatedTime(
    run(
      [
        [1, 0],
        [3, 1],
        [6, 1],
        [8, 2],
      ],
      [
        [8.01, 0],
        [9, 0.99],
      ],
    ),
    [T0_MS, T0_MS + 10_000],
  );
  const at = (seconds: number) => T0_MS + seconds * 1000;

  it("plays as long as the simulation ran, a pause taking none of it", () => {
    // 1 + 1 + 0 + 1 + 0.01 + 0.99 + 1 seconds.
    expect(time.lengthMs).toBeCloseTo(5_000);
    expect(time.covered).toBe(true);
  });

  it("counts received time before the clock and simulated time in it", () => {
    expect(time.played(at(0.5))).toBeCloseTo(500);
    expect(time.played(at(2))).toBeCloseTo(1_500);
    expect(time.played(at(4))).toBeCloseTo(2_000, 5);
    expect(time.played(at(7))).toBeCloseTo(2_500);
  });

  it("passes a pause at once, to where the simulation played again", () => {
    expect(time.receiveAt(1_999)).toBeCloseTo(at(2.998));
    expect(time.receiveAt(2_000)).toBeCloseTo(at(6));
    expect(time.receiveAt(2_500)).toBeCloseTo(at(7));
  });

  it("plays the segments after a reset in turn, and the time after the clock as received", () => {
    expect(time.receiveAt(3_010)).toBeCloseTo(at(8.01));
    expect(time.receiveAt(3_500)).toBeCloseTo(at(8.01 + 0.49 * (0.99 / 0.99)));
    expect(time.receiveAt(4_500)).toBeCloseTo(at(9.5));
    expect(time.receiveAt(5_000)).toBeCloseTo(at(10));
  });

  it("maps there and back to the same moment wherever the simulation moves", () => {
    for (const seconds of [0.2, 1.5, 2.9, 6.5, 7.9, 8.5, 9.7]) {
      expect(time.receiveAt(time.played(at(seconds)))).toBeCloseTo(at(seconds), 3);
    }
  });

  it("says the simulation's time, with the segment after a reset", () => {
    expect(simulatedLabel(time.at(at(2)))).toBe("0.5 s");
    expect(simulatedLabel(time.at(at(4)))).toBe("1.0 s");
    expect(simulatedLabel(time.at(at(8.505)))).toBe("0.5 s after reset 1");
    expect(simulatedLabel(time.at(at(0.5)))).toBe("—");
  });
});

describe("a run without the points of its segments", () => {
  it("covers nothing and plays as received", () => {
    const time = new SimulatedTime(run(), [T0_MS, T0_MS + 4_000]);
    expect(time.covered).toBe(false);
    expect(time.lengthMs).toBe(4_000);
    expect(time.receiveAt(1_000)).toBe(T0_MS + 1_000);
  });
});

describe("an axis in the simulation's time", () => {
  it("says seconds to a tenth unless its ticks are far apart, and the segment after a reset", () => {
    expect(axisSimulated({ segment: 0, seconds: 12.34 }, 2)).toBe("12.3 s");
    expect(axisSimulated({ segment: 0, seconds: 12.34 }, 30)).toBe("12 s");
    expect(axisSimulated({ segment: 2, seconds: 3 }, 1)).toBe("3.0 s\nafter reset 2");
    expect(axisSimulated(null, 1)).toBe("");
  });
});

describe("a replay's marks of its simulation", () => {
  it("are each pause, as a stretch, and each reset, as a moment", () => {
    const ran = run();
    ran.pauses = [{ start_utc_ns: T0 + 3 * S, end_utc_ns: T0 + 6 * S, simulated_ns: S }];
    ran.resets = [{ at_utc_ns: T0 + 8 * S, from_ns: 2 * S, to_ns: 0 }];
    const items = replayItems(null, null, null, null, ran).filter((item) => item.kind === SIMULATION_KIND);
    expect(items).toEqual([
      { kind: SIMULATION_KIND, startMs: T0_MS + 3_000, endMs: T0_MS + 6_000, label: "Simulation paused at 1.0 s" },
      { kind: SIMULATION_KIND, startMs: T0_MS + 8_000, endMs: T0_MS + 8_000, label: "Simulation reset from 2.0 s to 0.0 s" },
    ]);
  });
});
