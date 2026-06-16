import { describe, expect, it } from "vitest";

import { type PlotStream, plotStreams, resolvePlot, SampleBuffer } from "./plots";
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

describe("resolvePlot", () => {
  /** The sample arm's stream as the page plots it: seven joint positions, then the gripper. */
  const arm: PlotStream = {
    name: "arm_joints",
    index: 0,
    fields: [
      {
        name: "joint_positions",
        values: [2, 3, 4, 5, 6, 7, 8],
        names: Array.from({ length: 7 }, (_, place) => `joint_positions[${place}]`),
      },
      { name: "gripper", values: [9], names: ["gripper"] },
    ],
    samples: new SampleBuffer(10),
  };
  const base: PlotStream = { name: "base", index: 1, fields: [{ name: "speed", values: [0], names: ["speed"] }], samples: new SampleBuffer(1) };
  const roles = { "gripper.opening": "arm_joints.gripper", "joints.joint_2.position": "arm_joints.joint_positions[1]" };

  it("draws a whole field, one value of an array, or a role's field, each stream's together", () => {
    const parts = resolvePlot(
      ["arm_joints.joint_positions[6]", "base.speed", "role:gripper.opening", "role:joints.joint_2.position"],
      roles,
      [arm, base],
    );
    expect(parts.map((part) => [part.stream.name, part.values, part.labels])).toEqual([
      ["arm_joints", [8, 9, 3], ["joint_positions[6]", "gripper.opening", "joints.joint_2.position"]],
      ["base", [0], ["speed"]],
    ]);
    expect(resolvePlot(["arm_joints.joint_positions"], roles, [arm])[0]?.values).toEqual([2, 3, 4, 5, 6, 7, 8]);
  });

  it("leaves out what names nothing plotted: no such stream, field, value or role", () => {
    expect(
      resolvePlot(["wrist.force", "arm_joints.torque", "arm_joints.joint_positions[7]", "role:gripper.force"], roles, [arm]),
    ).toEqual([]);
  });
});
