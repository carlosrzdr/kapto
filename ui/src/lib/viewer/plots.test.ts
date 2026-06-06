import { describe, expect, it } from "vitest";

import { plotStreams, SampleBuffer } from "./plots";
import type { Opening } from "./stream";

/** A sample of two values: the first at `value`, the second at its double. */
const sample = (value: number) => new Float64Array([value - 0.5, value + 0.5, 2 * value, 2 * value]);

describe("SampleBuffer", () => {
  it("never holds more than its capacity, the oldest going first", () => {
    const buffer = new SampleBuffer(2, 4);
    for (let index = 0; index < 10; index++) {
      buffer.push(1000 * index, sample(index));
    }
    expect(buffer.size).toBe(4);
    expect(buffer.newestMs).toBe(9000);
    const [times, firstLow, firstHigh, second] = buffer.stretch(0, [0, 1]);
    expect(Array.from(times!)).toEqual([6, 7, 8, 9]);
    expect(Array.from(firstLow!)).toEqual([5.5, 6.5, 7.5, 8.5]);
    expect(Array.from(firstHigh!)).toEqual([6.5, 7.5, 8.5, 9.5]);
    expect(Array.from(second!)).toEqual([12, 14, 16, 18]);
  });

  it("hands out only the stretch asked for, and only the values asked for", () => {
    const buffer = new SampleBuffer(2, 8);
    [1000, 2000, 3000].forEach((time, index) => buffer.push(time, sample(index)));
    const stretch = buffer.stretch(2000, [1]);
    expect(stretch).toHaveLength(3);
    expect(Array.from(stretch[0]!)).toEqual([2, 3]);
    expect(Array.from(stretch[1]!)).toEqual([2, 4]);
    buffer.clear();
    expect(buffer.newestMs).toBeNull();
    expect(Array.from(buffer.stretch(0, [0])[0]!)).toEqual([]);
  });
});

describe("plotStreams", () => {
  it("gives each stream a plot per field, an array's values together, leaving out counters, clocks and streams without values", () => {
    const opening: Opening = {
      version: 1,
      links: [],
      history_s: 600,
      streams: [
        { name: "clock_only", values: [{ name: "sequence", plotted: false }] },
        {
          name: "arm_joints",
          values: [
            { name: "sequence", plotted: false },
            { name: "joint_positions[0]", plotted: true },
            { name: "joint_positions[1]", plotted: true },
            { name: "gripper", plotted: true },
          ],
        },
      ],
    };
    const [arm, ...rest] = plotStreams(opening);
    expect(rest).toEqual([]);
    expect(arm!.name).toBe("arm_joints");
    expect(arm!.index).toBe(1);
    expect(arm!.fields.map((field) => [field.name, field.values])).toEqual([
      ["joint_positions", [1, 2]],
      ["gripper", [3]],
    ]);
    expect(arm!.samples.values).toBe(4);
  });
});
