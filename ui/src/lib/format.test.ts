import { describe, expect, it } from "vitest";

import { breakable, bytes, cores, count, duration, figure, milliseconds, plain, rate } from "./format";

describe("format", () => {
  it("shows durations as minutes and seconds, with hours only when needed", () => {
    expect(duration(0)).toBe("0:00");
    expect(duration(75)).toBe("1:15");
    expect(duration(3725)).toBe("1:02:05");
    expect(duration(-3)).toBe("0:00");
  });

  it("separates thousands with thin spaces", () => {
    expect(count(999)).toBe("999");
    expect(count(1234567)).toBe("1 234 567");
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

  it("shows CPU use as a share of one core", () => {
    expect(cores(0.26)).toBe("26 %");
    expect(cores(1.5)).toBe("150 %");
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
