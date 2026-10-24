import { describe, expect, it } from "vitest";

import { type Drawn, drawAgain, type PlotStream, plotStreams, resolvePlot, SampleBuffer, shortLabels, shownStretch, valuesAt } from "./plots";
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

describe("shortLabels", () => {
  it("says what every label shares once, each keeping what tells it apart", () => {
    expect(shortLabels(["joints.joint_1.position", "joints.joint_2.position", "joints.gripper.position"])).toEqual({
      heading: "joints · position",
      labels: ["joint_1", "joint_2", "gripper"],
    });
    expect(shortLabels(["gripper.opening", "gripper.force"])).toEqual({ heading: "gripper", labels: ["opening", "force"] });
  });

  it("keeps a single label, and labels sharing nothing, as they are", () => {
    expect(shortLabels(["gripper.opening"])).toEqual({ heading: "", labels: ["gripper.opening"] });
    expect(shortLabels(["joint_positions[0]", "gripper"])).toEqual({ heading: "", labels: ["joint_positions[0]", "gripper"] });
    expect(shortLabels(["a.b", "a.b"])).toEqual({ heading: "a", labels: ["b", "b"] });
  });
});

describe("shownStretch", () => {
  // 30 s across 600 pixels: 50 ms a pixel.
  it("shows the span, ending within a pixel of the newest sample", () => {
    const [from, to] = shownStretch(1_000_037, 30_000, 600);
    expect(to - from).toBeCloseTo(30);
    expect(to).toBe(1000);
    expect(1_000_037 - to * 1000).toBeLessThan(50);
  });

  it("stays put while samples arrive within the same pixel", () => {
    expect(shownStretch(1_000_001, 30_000, 600)).toEqual(shownStretch(1_000_049, 30_000, 600));
  });

  it("moves by whole pixels as samples arrive", () => {
    const [, before] = shownStretch(1_000_049, 30_000, 600);
    const [, after] = shownStretch(1_000_149, 30_000, 600);
    expect((after - before) * 1000).toBeCloseTo(100);
  });

  it("ends at the newest sample before the plot has a width", () => {
    const [from, to] = shownStretch(1_000_037, 30_000, 0);
    expect(from).toBeCloseTo(970.037);
    expect(to).toBeCloseTo(1000.037);
  });
});

describe("valuesAt", () => {
  // Two samples of two values: each a minimum and a maximum.
  const at = new Date(2026, 2, 10, 14, 46, 5, 250).getTime() / 1000;
  const data = [
    [at - 1, at],
    [0.5, 0.815],
    [0.5, 0.818],
    [-2, 1200.4],
    [-1, 1500.6],
  ];
  // As a chart drawing one unit over 10 pixels.
  const tenPerUnit = (low: number, high: number) => (high - low) * 10;

  it("says the sample's time to the millisecond and each value by its label", () => {
    expect(valuesAt(data, 1, ["joint_1", "load"], tenPerUnit)).toEqual({
      time: "14:46:05.250",
      values: [
        { label: "joint_1", value: "0.817" },
        { label: "load", value: "1\u202f200 to 1\u202f501" },
      ],
    });
  });

  it("gives one value, the middle, while the range is too narrow to be more than the line", () => {
    // 0.003 apart is 0.03 pixels: a joint moving smoothly.
    expect(valuesAt(data, 1, ["joint_1"], tenPerUnit).values[0]?.value).toBe("0.817");
    // The same range drawn 1000 pixels a unit spans 3: a jump worth saying.
    expect(valuesAt(data, 1, ["joint_1"], (low, high) => (high - low) * 1000).values[0]?.value).toBe("0.815 to 0.818");
  });

  it("gives the range of a jump within the sample", () => {
    expect(valuesAt(data, 0, ["joint_1", "load"], tenPerUnit).values[1]).toEqual({ label: "load", value: "-2 to -1" });
  });

  it("says nothing it doesn't have", () => {
    expect(valuesAt(data, 5, ["joint_1"], tenPerUnit)).toEqual({ time: "–", values: [{ label: "joint_1", value: "–" }] });
  });
});

describe("drawAgain", () => {
  const stream = {};
  const last: Drawn = { toMs: 1_000_000, widthPx: 600, spanMs: 30_000, stream, newestMs: 1_000_030, atMs: 0 };

  it("draws the first time, and when the stretch moves a pixel or the chart changes", () => {
    expect(drawAgain(undefined, last)).toBe(true);
    expect(drawAgain(last, { ...last, toMs: 1_000_050, atMs: 33 })).toBe(true);
    expect(drawAgain(last, { ...last, widthPx: 601, atMs: 33 })).toBe(true);
    expect(drawAgain(last, { ...last, spanMs: 10_000, atMs: 33 })).toBe(true);
    expect(drawAgain(last, { ...last, stream: {}, atMs: 33 })).toBe(true);
  });

  it("doesn't draw for samples after the stretch's end, which are off the chart", () => {
    expect(drawAgain(last, { ...last, newestMs: 1_000_045, atMs: 33 })).toBe(false);
  });

  it("draws for a sample inside the stretch, as while looking back", () => {
    const behind = { ...last, newestMs: 990_000 };
    expect(drawAgain(behind, { ...behind, newestMs: 995_000, atMs: 33 })).toBe(true);
  });

  it("draws at least twice a second whatever changed", () => {
    expect(drawAgain(last, { ...last, atMs: 499 })).toBe(false);
    expect(drawAgain(last, { ...last, atMs: 500 })).toBe(true);
  });
});
