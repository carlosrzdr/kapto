import { describe, expect, it } from "vitest";

import {
  ALL_MS,
  asked,
  banded,
  BEHIND_MS,
  choose,
  type Drawn,
  drawAgain,
  FILLED_FIRST_MS,
  follow,
  plotColumns,
  type PlotStream,
  plotStreams,
  resolvePlot,
  SampleBuffer,
  shortLabels,
  stretchShown,
  valuesAt,
} from "./plots";
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
    expect(buffer.oldestMs).toBe(6000);
    const [times, firstLow, firstHigh, second] = buffer.stretch(0, [0, 1]);
    expect(Array.from(times!)).toEqual([6, 7, 8, 9]);
    expect(Array.from(firstLow!)).toEqual([5.5, 6.5, 7.5, 8.5]);
    expect(Array.from(firstHigh!)).toEqual([6.5, 7.5, 8.5, 9.5]);
    expect(Array.from(second!)).toEqual([12, 14, 16, 18]);
  });

  it("merges a live sample into the part of a stretch it falls in, and leaves out one it holds already", () => {
    const buffer = new SampleBuffer(2, 8);
    buffer.pushInto(1_010, sample(1), 100);
    buffer.pushInto(1_050, sample(3), 100);
    buffer.pushInto(1_040, sample(9), 100);
    buffer.pushInto(1_120, sample(2), 100);
    expect(buffer.size).toBe(2);
    expect(buffer.newestMs).toBe(1_120);
    const [times, firstLow, firstHigh, second] = buffer.stretch(0, [0, 1]);
    expect(Array.from(times!)).toEqual([1.05, 1.12]);
    // The first part: 1 and 3 merged, each value's lowest and highest.
    expect(Array.from(firstLow!)).toEqual([0.5, 1.5]);
    expect(Array.from(firstHigh!)).toEqual([3.5, 2.5]);
    expect(Array.from(second!)).toEqual([2, 4]);
  });

  it("hands out the sample just before the stretch too when asked, for a line to run to the edge", () => {
    const buffer = new SampleBuffer(2, 8);
    [1000, 2000, 3000].forEach((time, index) => buffer.push(time, sample(index)));
    expect(Array.from(buffer.stretch(2500, [0], true)[0]!)).toEqual([2, 3]);
    expect(Array.from(buffer.stretch(2000, [0], true)[0]!)).toEqual([1, 2, 3]);
    expect(Array.from(buffer.stretch(500, [0], true)[0]!)).toEqual([1, 2, 3]);
    expect(Array.from(buffer.stretch(2500, [0])[0]!)).toEqual([3]);
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

describe("stretchShown", () => {
  it("fills a stretch from the oldest sample, drawn from the left edge, while they are fewer", () => {
    expect(stretchShown(30_000, 100_000, 100_000)).toEqual({ endMs: 105_000, spanMs: FILLED_FIRST_MS });
    expect(stretchShown(30_000, 103_000, 100_000)).toEqual({ endMs: 105_000, spanMs: FILLED_FIRST_MS });
  });

  it("zooms out from there with the samples kept, the oldest at the left edge, until they reach back the whole span", () => {
    expect(stretchShown(30_000, 105_000, 100_000)).toEqual({ endMs: 105_000, spanMs: 5_000 });
    expect(stretchShown(30_000, 112_500, 100_000)).toEqual({ endMs: 112_500, spanMs: 12_500 });
    expect(stretchShown(30_000, 130_000, 100_000)).toEqual({ endMs: 130_000, spanMs: 30_000 });
  });

  it("never jumps: a few milliseconds more change it by as much, filling, zooming or moving on", () => {
    for (const end of [101_000, 104_990, 110_000, 129_990, 150_000]) {
      const now = stretchShown(30_000, end, 100_000);
      const next = stretchShown(30_000, end + 10, 100_000);
      expect(Math.abs(next.endMs - now.endMs)).toBeLessThanOrEqual(10);
      expect(Math.abs(next.spanMs - now.spanMs)).toBeLessThanOrEqual(10);
    }
  });

  it("shows the whole span once the samples kept reach back that far", () => {
    expect(stretchShown(30_000, 100_000, 60_000)).toEqual({ endMs: 100_000, spanMs: 30_000 });
  });

  it("fills a span shorter than the first stretch as it is", () => {
    expect(stretchShown(3_000, 101_000, 100_000)).toEqual({ endMs: 103_000, spanMs: 3_000 });
  });

  it("with all there is asked for, zooms out over everything kept, however long", () => {
    expect(stretchShown(ALL_MS, 102_000, 100_000)).toEqual({ endMs: 105_000, spanMs: FILLED_FIRST_MS });
    expect(stretchShown(ALL_MS, 700_000, 100_000)).toEqual({ endMs: 700_000, spanMs: 600_000 });
  });

  it("shows nothing before an episode's start while it is recorded, filling from there", () => {
    expect(stretchShown(ALL_MS, 160_000, 100_000, 158_000)).toEqual({ endMs: 163_000, spanMs: FILLED_FIRST_MS });
    expect(stretchShown(ALL_MS, 400_000, 100_000, 158_000)).toEqual({ endMs: 400_000, spanMs: 242_000 });
    // An episode longer than what is kept: from the oldest kept.
    expect(stretchShown(ALL_MS, 400_000, 200_000, 158_000)).toEqual({ endMs: 400_000, spanMs: 200_000 });
    // Looked back to before it started: as if it hadn't.
    expect(stretchShown(ALL_MS, 150_000, 100_000, 158_000)).toEqual({ endMs: 150_000, spanMs: 50_000 });
  });

  it("shows the whole span to the end when nothing is kept, or what is kept starts after the end", () => {
    expect(stretchShown(30_000, 100_000, null)).toEqual({ endMs: 100_000, spanMs: 30_000 });
    expect(stretchShown(30_000, 100_000, 120_000)).toEqual({ endMs: 100_000, spanMs: 30_000 });
    expect(stretchShown(ALL_MS, 100_000, null)).toEqual({ endMs: 100_000, spanMs: FILLED_FIRST_MS });
  });
});

describe("follow", () => {
  /** How far Kapto's clock is ahead of the page's. */
  const KAPTO = 1_000_000;

  it("trails the newest sample, moving on with every frame drawn rather than as samples arrive", () => {
    let { following, endMs } = follow(undefined, KAPTO + 1_000, 1_000);
    expect(endMs).toBe(KAPTO + 1_000 - BEHIND_MS);
    // Frames drawn between samples: the end moves on by the time between them.
    const ends = [1_005, 1_010, 1_015].map((now) => {
      ({ following, endMs } = follow(following, KAPTO + 1_000, now));
      return endMs - (KAPTO + 1_000 - BEHIND_MS);
    });
    expect(ends).toEqual([5, 10, 15]);
  });

  it("keeps time by the soonest sample to arrive, so a late one doesn't hold it back", () => {
    let { following } = follow(undefined, KAPTO + 1_000, 1_000);
    // Arriving 40 ms late: Kapto's clock is still taken as it was.
    ({ following } = follow(following, KAPTO + 1_016, 1_056));
    expect(following.offsetMs).toBe(KAPTO);
    // Arriving sooner than any before: taken from it.
    ({ following } = follow(following, KAPTO + 1_100, 1_090));
    expect(following.offsetMs).toBe(KAPTO + 10);
  });

  it("never ends past the newest sample, so a stream that stops isn't drawn on into nothing", () => {
    const { following } = follow(undefined, KAPTO + 1_000, 1_000);
    expect(follow(following, KAPTO + 1_000, 1_000 + BEHIND_MS + 5_000).endMs).toBe(KAPTO + 1_000);
  });

  it("takes the clock again when the newest sample goes back in time, as when a stream starts again", () => {
    const { following } = follow(undefined, KAPTO + 5_000, 1_000);
    const again = follow(following, KAPTO + 2_000, 1_100);
    expect(again.following.offsetMs).toBe(KAPTO + 900);
    expect(again.endMs).toBe(KAPTO + 2_000 - BEHIND_MS);
  });
});

describe("asked", () => {
  it("asks for parts of a round length starting on its multiples, so asking again a second later merges the same", () => {
    const first = asked(1_000_013, 1_600_013, 880);
    expect(first.partMs).toBe(1_000);
    expect(first.fromMs % first.partMs).toBe(0);
    expect(first.toMs % first.partMs).toBe(0);
    expect(first.fromMs).toBeLessThanOrEqual(1_000_013);
    expect(first.toMs).toBeGreaterThanOrEqual(1_600_013);
    expect(first.points).toBe((first.toMs - first.fromMs) / first.partMs);
    // A second later, the parts it shares with the first start at the same moments.
    const next = asked(1_001_013, 1_601_013, 880);
    expect(next.partMs).toBe(first.partMs);
    expect((next.fromMs - first.fromMs) % first.partMs).toBe(0);
  });

  it("takes the shortest round length giving no more parts than asked for", () => {
    expect(asked(0, 30_000, 880).partMs).toBe(50);
    expect(asked(0, 600_000, 880).partMs).toBe(1_000);
    expect(asked(0, 100_000_000, 880).partMs).toBe(60_000);
  });
});

describe("banded", () => {
  it("draws the band of each value shown while two or fewer are", () => {
    expect(banded([true])).toEqual([true]);
    expect(banded([true, true])).toEqual([true, true]);
    expect(banded([false, true, false, true])).toEqual([false, true, false, true]);
  });

  it("draws no band while more than two are shown, nor a hidden value's", () => {
    expect(banded([true, true, true, false])).toEqual([false, false, false, false]);
    expect(banded([false, false])).toEqual([false, false]);
  });
});

describe("choose", () => {
  it("shows a value alone once its entry is clicked, and another alone once that one is", () => {
    expect(choose(null, 4, 2, false)).toEqual(new Set([2]));
    expect(choose(new Set([2]), 4, 0, false)).toEqual(new Set([0]));
    expect(choose(new Set([1, 3]), 4, 1, false)).toEqual(new Set([1]));
  });

  it("shows every value again once the one shown alone is clicked", () => {
    expect(choose(new Set([2]), 4, 2, false)).toBeNull();
  });

  it("adds a value to those shown, or takes it away, on a click with a modifier", () => {
    expect(choose(new Set([2]), 4, 0, true)).toEqual(new Set([0, 2]));
    expect(choose(new Set([0, 2]), 4, 0, true)).toEqual(new Set([2]));
    expect(choose(null, 4, 1, true)).toEqual(new Set([0, 2, 3]));
  });

  it("shows every value again rather than none, and says so once every one is", () => {
    expect(choose(new Set([2]), 4, 2, true)).toBeNull();
    expect(choose(new Set([0, 1, 2]), 4, 3, true)).toBeNull();
  });
});

describe("plotColumns", () => {
  it("gives each value's middle, minima and maxima, the middle written into what it is given", () => {
    const buffer = new SampleBuffer(2, 4);
    buffer.push(1000, sample(1));
    buffer.push(2000, sample(2));
    const middles = [new Float64Array(4), new Float64Array(4)];
    const columns = plotColumns(buffer.stretch(0, [0, 1]), [true, true], middles);
    expect(columns.map((column) => Array.from(column))).toEqual([
      [1, 2],
      [1, 2],
      [0.5, 1.5],
      [1.5, 2.5],
      [2, 4],
      [2, 4],
      [2, 4],
    ]);
    expect(columns[4]!.buffer).toBe(middles[1]!.buffer);
  });

  it("leaves the middle of a value not drawn as a line as it was", () => {
    const buffer = new SampleBuffer(1, 4);
    buffer.push(1000, new Float64Array([0.5, 1.5]));
    const middles = [new Float64Array([7, 7, 7, 7])];
    const columns = plotColumns(buffer.stretch(0, [0]), [false], middles);
    expect(Array.from(columns[1]!)).toEqual([7]);
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

  it("doesn't draw for a move too small to see, and draws once the moves add up to it", () => {
    // 30 s across 600 pixels: 50 ms a pixel, so a quarter pixel is 12.5 ms.
    expect(drawAgain(last, { ...last, toMs: 1_000_010, atMs: 16 })).toBe(false);
    expect(drawAgain(last, { ...last, toMs: 1_000_013, atMs: 33 })).toBe(true);
    // Zooming out moves the start alone.
    expect(drawAgain(last, { ...last, spanMs: 30_013, atMs: 33 })).toBe(true);
  });

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
