import { fireEvent, render, screen } from "@testing-library/svelte";
import { describe, expect, it, vi } from "vitest";

import type { PreflightCheck, PreflightRun } from "./api-types";
import PreflightPanel from "./PreflightPanel.svelte";

function check(code: string, passed: boolean, stream: string | null = null, waived = false): PreflightCheck {
  return {
    code,
    stream,
    passed,
    waived,
    summary: passed ? "" : `${code} summary`,
    remedy: `${code} remedy`,
    measured: "0 messages in 5.0 s, 0/s",
    needed: "messages arriving",
  };
}

function run(...checks: PreflightCheck[]): PreflightRun {
  return {
    ran_at: "2026-03-16T07:00:00.000Z",
    window_ms: 5000,
    passed: checks.every((c) => c.passed),
    checks,
  };
}

const silentRun = run(
  check("preflight.disk_too_small", true),
  check("preflight.stream_silent", false, "arm_joints"),
  check("preflight.stream_silent", false, "gripper"),
);

interface Overrides {
  run?: PreflightRun | null;
  recording?: boolean;
  running?: boolean;
  [key: string]: unknown;
}

function props(overrides: Overrides = {}) {
  return {
    run: silentRun,
    recording: false,
    running: false,
    busy: false,
    onrun: vi.fn(),
    onrecordanyway: vi.fn(),
    ...overrides,
  };
}

describe("PreflightPanel", () => {
  it("says a pass in one quiet line, every check on hover, and offers only to run it again", async () => {
    const onrun = vi.fn();
    render(PreflightPanel, props({ run: run(check("preflight.disk_too_small", true)), onrun }));
    const line = screen.getByText(/^Passed at/);
    expect(line.textContent).toMatch(/^Passed at .*\d\d:\d\d:\d\d$/);
    expect(line.title).toBe("All 1 checks passed: preflight.disk_too_small");
    expect(screen.queryByRole("list")).toBeNull();
    expect(screen.queryByText(/checks failed/)).toBeNull();
    expect(screen.queryByRole("button", { name: "Record anyway" })).toBeNull();
    await fireEvent.click(screen.getByRole("button", { name: "Run again" }));
    expect(onrun).toHaveBeenCalledOnce();
  });

  it("shows each failed check with its stream and remedy, and records anyway waiving exactly what failed", async () => {
    const onrecordanyway = vi.fn();
    render(PreflightPanel, props({ onrecordanyway }));
    expect(screen.getByText(/2 of 3 checks failed/)).toBeTruthy();
    expect(screen.queryByText(/^Passed at/)).toBeNull();
    expect(screen.getAllByText("preflight.stream_silent summary")).toHaveLength(2);
    expect(screen.getByText(/arm_joints/)).toBeTruthy();
    expect(screen.getByText(/gripper/)).toBeTruthy();
    expect(screen.getAllByText("preflight.stream_silent remedy")).toHaveLength(2);
    expect(screen.queryByText("preflight.disk_too_small remedy")).toBeNull();
    await fireEvent.click(screen.getByRole("button", { name: "Record anyway" }));
    expect(onrecordanyway).toHaveBeenCalledWith(["preflight.stream_silent"]);
  });

  it("while it runs or another order is on its way, nothing more can be asked", () => {
    render(PreflightPanel, props({ running: true }));
    expect((screen.getByRole("button", { name: "Running…" }) as HTMLButtonElement).disabled).toBe(true);
    expect((screen.getByRole("button", { name: "Record anyway" }) as HTMLButtonElement).disabled).toBe(true);
  });

  it("while recording, says what the episode was started despite, with no buttons", () => {
    const waived = run(check("preflight.stream_silent", false, "arm_joints", true));
    render(PreflightPanel, props({ run: waived, recording: true }));
    expect(screen.getByText(/Recording despite 1 failed check/)).toBeTruthy();
    expect(screen.getByText("waived")).toBeTruthy();
    expect(screen.queryByRole("button")).toBeNull();
  });

  it("while recording after a pass, says only that it passed, with no buttons", () => {
    render(PreflightPanel, props({ run: run(check("preflight.disk_too_small", true)), recording: true }));
    expect(screen.getByText(/^Passed at/)).toBeTruthy();
    expect(screen.queryByRole("button")).toBeNull();
  });

  it("before the first run has ended, says so", () => {
    render(PreflightPanel, props({ run: null }));
    expect(screen.getByText(/Not run yet/)).toBeTruthy();
    expect(screen.getByRole("button", { name: "Run again" })).toBeTruthy();
  });
});
