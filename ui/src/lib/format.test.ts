import { describe, expect, it } from "vitest";

import { axisTimes, axisTimesSpace, breakable, bytes, cores, count, duration, figure, milliseconds, plain, rate, sentence } from "./format";

describe("format", () => {
  it("shows durations as minutes and seconds, with hours only when needed", () => {
    expect(duration(0)).toBe("0:00");
    expect(duration(75)).toBe("1:15");
    expect(duration(3725)).toBe("1:02:05");
    expect(duration(-3)).toBe("0:00");
  });

  it("separates thousands with narrow spaces no line breaks at", () => {
    expect(count(999)).toBe("999");
    expect(count(1234567)).toBe("1 234 567");
  });

  it("gives no rate when no time passed or a counter went back to zero", () => {
    expect(rate(1000, 3000, 1)).toBe(2000);
    expect(rate(1000, 3000, 0)).toBe(0);
    expect(rate(3000, 10, 1)).toBe(0);
  });

  it("shows sizes in binary units, with a decimal below ten", () => {
    expect(bytes(512)).toBe("512 B");
    expect(bytes(87_203_840)).toBe("83 MiB");
    expect(bytes(1_610_612_736)).toBe("1.5 GiB");
    expect(bytes(-1)).toBe("0 B");
  });

  it("shows CPU use in cores, never as a percentage of the whole machine", () => {
    expect(cores(0.26)).toBe("0.26 cores");
    expect(cores(1.5)).toBe("1.50 cores");
  });

  it("shows short times in milliseconds and long ones in seconds", () => {
    expect(milliseconds(520_000_000)).toBe("520 ms");
    expect(milliseconds(1_500_000)).toBe("1.5 ms");
    expect(milliseconds(12_000_000_000)).toBe("12 s");
  });

  it("splits names and paths after each dot, underscore and slash, keeping every character", () => {
    expect(breakable("arm_joints.joint_positions[0]")).toEqual(["arm_", "joints.", "joint_", "positions[0]"]);
    expect(breakable("/tmp/kapto")).toEqual(["/", "tmp/", "kapto"]);
    expect(breakable("info")).toEqual(["info"]);
  });

  it("shows a measured value to three significant figures, and large ones as counts", () => {
    expect(figure(0.012345)).toBe("0.0123");
    expect(figure(1.5)).toBe("1.5");
    expect(figure(-2.0004)).toBe("-2");
    expect(figure(999.4)).toBe("999");
    expect(figure(12345.6)).toBe(count(12346));
    expect(figure(Number.NaN)).toBe("–");
  });

  it("leaves the operating system's error numbers out of a message", () => {
    expect(plain("the folder `/srv/nas`: Not a directory (os error 20)")).toBe("the folder `/srv/nas`: Not a directory");
    expect(plain("refused")).toBe("refused");
  });
});

describe("axisTimes", () => {
  // Local times, as the page shows them: built from the parts, so the test
  // holds in any time zone.
  const at = (day: number, hours: number, minutes: number, seconds: number, ms = 0) =>
    new Date(2026, 9, day, hours, minutes, seconds, ms).getTime() / 1000;

  it("writes each tick in 24-hour time, the date under the first only", () => {
    expect(axisTimes([at(26, 14, 45, 50), at(26, 14, 45, 55), at(26, 14, 46, 0)], 5)).toEqual([
      "14:45:50\n2026-10-26",
      "14:45:55",
      "14:46:00",
    ]);
  });

  it("drops the seconds for ticks minutes apart, and the time for ticks days apart", () => {
    expect(axisTimes([at(26, 9, 0, 0), at(26, 9, 30, 0)], 1_800)).toEqual(["09:00\n2026-10-26", "09:30"]);
    expect(axisTimes([at(26, 0, 0, 0), at(27, 0, 0, 0)], 86_400)).toEqual(["2026-10-26", "2026-10-27"]);
  });

  it("writes fractions of a second for ticks closer than a second, and the date again past midnight", () => {
    expect(axisTimes([at(26, 23, 59, 59, 750), at(27, 0, 0, 0)], 0.25)).toEqual([
      "23:59:59.75\n2026-10-26",
      "00:00:00.00\n2026-10-27",
    ]);
  });
});


describe("axisTimesSpace", () => {
  it("keeps ticks as far apart as the widest clock written, and a gap", () => {
    // Seven pixels a character, as a monospace font of about 11px.
    const measure = (text: string) => text.length * 7;
    expect(axisTimesSpace(measure)).toBe(12 * 7 + 12);
    for (const label of axisTimes([1_793_101_772.25, 1_793_101_772.5], 0.25)) {
      for (const line of label.split("\n")) {
        expect(measure(line)).toBeLessThan(axisTimesSpace(measure));
      }
    }
  });
});

describe("sentence", () => {
  it("ends a message with a full stop, so the remedy after it reads as the next sentence", () => {
    expect(sentence("Can't read the mesh file: meshes/gripper.stl")).toBe("Can't read the mesh file: meshes/gripper.stl.");
    expect(sentence("The kind of event isn't one that may be sent.")).toBe("The kind of event isn't one that may be sent.");
    expect(sentence("Is it there? ")).toBe("Is it there? ");
  });
});
