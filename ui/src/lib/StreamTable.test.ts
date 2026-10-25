import { render, screen } from "@testing-library/svelte";
import { describe, expect, it } from "vitest";

import StreamTable from "./StreamTable.svelte";

const arm = {
  stream: "arm_joints",
  received: 14000,
  written: 13990,
  dropped_by_us: 10,
  lost_upstream: 0,
};

describe("StreamTable", () => {
  it("shows each stream's rate and counts, and marks drops as a problem", () => {
    render(StreamTable, { streams: [arm], rates: { arm_joints: 2000 }, recording: true });
    const row = screen.getByRole("row", { name: /arm_joints/ });
    expect(row.textContent).toContain("2 000/s");
    expect(row.textContent).toContain("13 990");
    const cells = row.querySelectorAll("td");
    expect(cells[3]?.classList.contains("problem")).toBe(true);
    expect(cells[4]?.classList.contains("problem")).toBe(false);
  });

  it("shows no loss quietly, as a muted dash a screen reader reads as none, and any loss in the danger colour", () => {
    render(StreamTable, {
      streams: [arm, { ...arm, stream: "gripper", dropped_by_us: 0, lost_upstream: 3 }],
      rates: {},
      recording: true,
    });
    const quiet = screen.getByRole("row", { name: /arm_joints/ }).querySelectorAll("td")[4];
    expect(quiet?.textContent).toBe("–none");
    expect(quiet?.querySelector("[aria-hidden='true']")?.textContent).toBe("–");
    expect(quiet?.classList.contains("problem")).toBe(false);
    const cells = screen.getByRole("row", { name: /gripper/ }).querySelectorAll("td");
    expect(cells[3]?.textContent).toBe("–none");
    expect(cells[4]?.textContent).toBe("3");
    expect(cells[4]?.classList.contains("problem")).toBe(true);
    expect(cells[4]?.classList.contains("loss")).toBe(true);
    // The drops above zero say their number, as loud.
    const dropped = screen.getByRole("row", { name: /arm_joints/ }).querySelectorAll("td")[3];
    expect(dropped?.textContent).toBe("10");
    expect(dropped?.classList.contains("loss")).toBe(true);
  });

  it("shows no written count while nothing is being recorded", () => {
    render(StreamTable, { streams: [{ ...arm, written: 0 }], rates: {}, recording: false });
    const cells = screen.getByRole("row", { name: /arm_joints/ }).querySelectorAll("td");
    expect(cells[2]?.textContent).toBe("–");
  });

  it("says so when no streams are declared", () => {
    render(StreamTable, { streams: [], rates: {}, recording: false });
    expect(screen.getByText("No streams declared.")).toBeTruthy();
  });

  it("says the streams aren't known while capture can't be reached, rather than that there are none", () => {
    render(StreamTable, { streams: [], rates: {}, recording: false, known: false });
    expect(screen.getByText("Not known while Kapto can't reach the capture process.")).toBeTruthy();
    expect(screen.queryByText("No streams declared.")).toBeNull();
  });

  it("marks a stream that has gone silent", () => {
    render(StreamTable, { streams: [arm], rates: { arm_joints: 0 }, recording: false });
    const rate = screen.getByRole("row", { name: /arm_joints/ }).querySelectorAll("td")[0];
    expect(rate?.textContent?.trim()).toBe("silent");
    expect(rate?.classList.contains("problem")).toBe(true);
    expect(screen.getByRole("img", { name: "nothing arriving" })).toBeTruthy();
  });

  it("shows a stream kept for late subscribers as kept, not silent, when nothing arrives", () => {
    render(StreamTable, {
      streams: [{ ...arm, stream: "tf_static" }],
      rates: { tf_static: 0 },
      recording: false,
      kept: { tf_static: true },
    });
    const rate = screen.getByRole("row", { name: /tf_static/ }).querySelectorAll("td")[0];
    expect(rate?.textContent?.trim()).toBe("kept");
    expect(rate?.classList.contains("problem")).toBe(false);
    expect(
      screen.getByRole("img", { name: "sends its messages once; every episode starts with the newest" }),
    ).toBeTruthy();
  });

  it("marks a stream well below its declared rate, and says which rate that is", () => {
    render(StreamTable, {
      streams: [arm],
      rates: { arm_joints: 1500 },
      recording: false,
      declared: { arm_joints: 2000 },
    });
    const rate = screen.getByRole("row", { name: /arm_joints/ }).querySelectorAll("td")[0];
    expect(rate?.textContent?.trim()).toMatch(/^1\s500\/s of 2\s000$/);
    expect(rate?.classList.contains("problem")).toBe(true);
  });

  it("shows a stream arriving at its declared rate as healthy", () => {
    render(StreamTable, {
      streams: [arm],
      rates: { arm_joints: 1990 },
      recording: false,
      declared: { arm_joints: 2000 },
    });
    expect(screen.getByRole("img", { name: "arriving at its declared rate" }).classList.contains("ok")).toBe(true);
  });

  it("shows no rate or health until the stream's rate is known", () => {
    render(StreamTable, { streams: [arm], rates: {}, recording: false });
    expect(screen.queryByRole("img")).toBeNull();
    const rate = screen.getByRole("row", { name: /arm_joints/ }).querySelectorAll("td")[0];
    expect(rate?.textContent?.trim()).toBe("–");
  });

  it("doesn't mark a stream close to its declared rate", () => {
    render(StreamTable, {
      streams: [arm],
      rates: { arm_joints: 1990 },
      recording: false,
      declared: { arm_joints: 2000 },
    });
    const rate = screen.getByRole("row", { name: /arm_joints/ }).querySelectorAll("td")[0];
    expect(rate?.classList.contains("problem")).toBe(false);
  });

  it("leaves what became of a camera's frames on their way to the live view to the camera's panel", () => {
    const camera = {
      stream: "wrist_camera",
      received: 210,
      written: 0,
      dropped_by_us: 0,
      lost_upstream: 0,
      live_view: { offered: 200, skipped: 0, superseded: 30, not_sent: 4, sent: 166 },
    };
    render(StreamTable, { streams: [arm, camera], rates: {}, recording: false });
    // Lossy on purpose, and not the recording's loss: not in the loss table.
    expect(screen.queryByText(/Live view:/)).toBeNull();
    expect(screen.getAllByRole("row")).toHaveLength(3);
  });
});
