import { describe, expect, it } from "vitest";

import { SLOW } from "./health";
import { RATE_READINGS, ratesOver, type Reading, withReading } from "./stream-rates";

const reading = (at: number, joints: number): Reading => ({ at, received: new Map([["joints", joints]]) });

const taken = (each: Reading[]): Reading[] => each.reduce<Reading[]>((readings, one) => withReading(readings, one), []);

describe("stream rates", () => {
  // A stream sending 2000 a second, as the page read it around Record in
  // the demo: the reading that saw the episode start was asked for while
  // the start went on, its counts taken later than it is dated.
  const aroundStart = [reading(37_350, 97_326), reading(38_350, 99_327), reading(38_860, 262), reading(39_350, 660), reading(40_350, 2_662)];

  it("start again from the reading after the one that saw an episode start, which reads low", () => {
    // Measured from the restart's reading, the stream would look slow.
    expect((2_662 - 262) / ((40_350 - 38_860) / 1000)).toBeLessThan(2000 * SLOW);
    const readings = taken(aroundStart);
    expect(readings).toEqual([reading(39_350, 660), reading(40_350, 2_662)]);
    expect(ratesOver(readings, ["joints"])).toEqual({ joints: 2002 });
  });

  it("are not measured until two readings since a restart, the rates before standing", () => {
    expect(ratesOver(taken(aroundStart.slice(0, 3)), ["joints"])).toBeNull();
    expect(ratesOver(taken(aroundStart.slice(0, 4)), ["joints"])).toBeNull();
  });

  it("are measured over the latest few readings", () => {
    const readings = taken(Array.from({ length: 8 }, (_, second) => reading(second * 1000, second * 2000)));
    expect(readings).toHaveLength(RATE_READINGS);
    expect(readings[0]).toEqual(reading(3000, 6000));
    expect(ratesOver(readings, ["joints"])).toEqual({ joints: 2000 });
  });

  it("say a stream missing from the readings sent nothing", () => {
    expect(ratesOver([reading(0, 0), reading(1000, 2000)], ["joints", "camera"])).toEqual({ joints: 2000, camera: 0 });
  });
});
