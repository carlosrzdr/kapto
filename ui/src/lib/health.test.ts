import { describe, expect, it } from "vitest";

import type { PreflightCheck, ProcessState, Status, Storage, StreamCounts } from "./api-types";
import { health, type HealthInput, PREFLIGHT, problems, RANK, streamState, UNREACHABLE } from "./health";

const GiB = 1024 ** 3;

const stream = (name: string, counts: Partial<StreamCounts> = {}): StreamCounts => ({
  stream: name,
  received: 1000,
  written: 0,
  dropped_by_us: 0,
  lost_upstream: 0,
  ...counts,
});

const check = (code: string, counts: Partial<PreflightCheck> = {}): PreflightCheck => ({
  code,
  summary: "",
  remedy: "",
  measured: "",
  needed: "",
  passed: false,
  waived: false,
  stream: "arm_joints",
  ...counts,
});

const storage = (free: number): Storage => ({
  data_dir: "/data",
  episodes: 0,
  episodes_bytes: 0,
  staging_bytes: 0,
  free_bytes: free,
  total_bytes: 100 * GiB,
  warn_below_bytes: 10 * GiB,
  stop_below_bytes: 2 * GiB,
});

const status = (streams: StreamCounts[], checks: PreflightCheck[] = []): Status => ({
  capture: {
    seconds: 10,
    streams,
    preflight: { checks, passed: checks.every((each) => each.passed), ran_at: "2026-03-16T07:00:00Z", window_ms: 5000 },
  },
  viewer: { stream_port: 7701 } as Status["viewer"],
});

/** All well: two streams at their declared rates, room on the disk, everything up. */
const well = (): HealthInput => ({
  status: status([stream("arm_joints"), stream("gripper")], [check("preflight.stream_silent", { passed: true })]),
  failure: null,
  rates: { arm_joints: 2000, gripper: 100 },
  declared: { arm_joints: 2000, gripper: 100 },
  kept: {},
  storage: storage(50 * GiB),
  processes: [{ name: "capture", state: "running", restarts: 0 }],
  destinations: [{ name: "archive", url: "file:///archive", uploaded: 3, uploading: 0, failed: 0, throttled: 0, bytes_sent: 0, bytes_total: 0 }],
});

describe("health", () => {
  it("says all is well in the quiet style while nothing needs attention", () => {
    expect(health(well())).toEqual({ level: "ok", text: "All well", more: 0, target: null, problems: [] });
  });

  it("says neither well nor wrong before the first status answers", () => {
    expect(health({ ...well(), status: null }).text).toBe("Checking…");
  });

  it("names the worst problem, how many more there are, in the colour of the worst", () => {
    const input = well();
    input.status = status([stream("arm_joints", { lost_upstream: 12 }), stream("gripper")]);
    input.rates.gripper = 0;
    input.storage = storage(4 * GiB);
    const said = health(input);
    expect(said).toMatchObject({ level: "danger", text: "arm_joints 12 lost", more: 2 });
    expect(said.target).toEqual({ tab: "live", anchor: "stream-arm_joints" });
    expect(said.problems.map((problem) => problem.text)).toEqual(["arm_joints 12 lost", "gripper silent", "Disk 4 % free"]);
  });

  it("names capture not answering before anything it said last", () => {
    const input = { ...well(), failure: { code: "capture.control_unreachable" } };
    input.status = status([stream("arm_joints", { dropped_by_us: 3 })]);
    input.processes = [{ name: "capture", state: "restarting" as ProcessState, restarts: 1 }];
    const said = health(input);
    expect(said).toMatchObject({ level: "danger", text: "Capture not reachable", more: 1 });
    expect(said.target).toEqual({ tab: "live", anchor: "live-streams" });
    expect(said.problems[1]).toEqual({ level: "warning", text: "capture restarting", target: { tab: "system", anchor: "process-capture" } });
  });

  it("ties: of one kind, the most lost first, then in the order the streams are declared", () => {
    const input = well();
    input.status = status([
      stream("a", { lost_upstream: 2 }),
      stream("b", { lost_upstream: 9 }),
      stream("c", { lost_upstream: 2 }),
    ]);
    expect(problems(input).map((problem) => problem.text)).toEqual(["b 9 lost", "a 2 lost", "c 2 lost"]);
  });

  it("says the disk in the danger colour once recording stops for lack of room", () => {
    expect(health({ ...well(), storage: storage(9 * GiB) })).toMatchObject({ level: "warning", text: "Disk 9 % free" });
    expect(health({ ...well(), storage: storage(1 * GiB) })).toMatchObject({ level: "danger", text: "Disk 1 % free" });
  });

  it("leaves out a pre-flight check the operator waived, and says one it doesn't know by its summary", () => {
    const input = well();
    input.status = status([stream("arm_joints"), stream("gripper")], [
      check("preflight.stream_silent", { waived: true }),
      check("preflight.something_new", { summary: "Something new failed" }),
    ]);
    expect(health(input)).toMatchObject({ level: "warning", text: "Pre-flight: Something new failed", more: 0 });
  });

  it("names a slow stream, a process restarting and a destination failing, and where each is shown", () => {
    const input = well();
    input.rates.arm_joints = 1500;
    input.processes = [{ name: "worker", state: "stopped", restarts: 0 }];
    input.destinations = [{ ...input.destinations[0]!, name: "nas", failed: 2 }];
    expect(problems(input)).toEqual([
      { level: "warning", text: "arm_joints slow", target: { tab: "live", anchor: "stream-arm_joints" } },
      { level: "warning", text: "worker stopped", target: { tab: "system", anchor: "process-worker" } },
      { level: "warning", text: "Uploads to nas failing", target: { tab: "uploads", anchor: "destination-nas" } },
    ]);
  });

  it("doesn't count a kept stream's silence, nor a stream whose rate isn't measured yet", () => {
    expect(streamState(0, 100, true)).toBe("kept");
    expect(streamState(undefined, 100, false)).toBeNull();
    expect(problems({ ...well(), rates: {} })).toEqual([]);
  });
});

describe("the ranking", () => {
  it("ranks every danger above every warning, each kind at a rank of its own", () => {
    const ranks = Object.values(RANK);
    const worstWarning = Math.min(...ranks.filter((each) => each.level === "warning").map((each) => each.rank));
    const bestDanger = Math.max(...ranks.filter((each) => each.level === "danger").map((each) => each.rank));
    expect(bestDanger).toBeLessThan(worstWarning);
    expect(new Set(ranks.map((each) => each.rank)).size).toBe(ranks.length);
  });

  it("gives every pre-flight check's cause code words of its own", () => {
    // `cargo xtask ci` checks this list against every `preflight.*` code Kapto has.
    for (const [code, said] of Object.entries(PREFLIGHT)) {
      const text = said(check(code));
      expect(text, code).toMatch(/^Pre-flight: /);
      expect(health({ ...well(), status: status([stream("arm_joints"), stream("gripper")], [check(code)]) }).text).toBe(text);
    }
  });

  it("names every way the status request fails that the page knows of", () => {
    for (const [code, text] of Object.entries(UNREACHABLE)) {
      expect(health({ ...well(), failure: { code } })).toMatchObject({ level: "danger", text });
    }
    expect(health({ ...well(), failure: { code: "ui.something_new" } }).text).toBe("Status not readable");
  });
});
