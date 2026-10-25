import { fireEvent, render, screen } from "@testing-library/svelte";
import { describe, expect, it, vi } from "vitest";

import HealthChip from "./HealthChip.svelte";
import type { Health } from "./health";

const allWell: Health = { level: "ok", text: "All well", more: 0, target: null, problems: [] };

const wrong: Health = {
  level: "danger",
  text: "arm_joints 12 lost",
  more: 2,
  target: { tab: "live", anchor: "stream-arm_joints" },
  problems: [
    { level: "danger", text: "arm_joints 12 lost", target: { tab: "live", anchor: "stream-arm_joints" } },
    { level: "warning", text: "gripper silent", target: { tab: "live", anchor: "stream-gripper" } },
    { level: "warning", text: "Disk 4 % free", target: { tab: "live", anchor: "live-storage" } },
  ],
};

describe("HealthChip", () => {
  it("says all is well quietly, a button a screen reader names and that opens the details", async () => {
    const onopen = vi.fn();
    render(HealthChip, { health: allWell, onopen });
    const chip = screen.getByRole("button", { name: "Health: All well" });
    expect(chip.classList.contains("ok")).toBe(true);
    expect(screen.getByRole("button", { description: /nothing needs attention/ })).toBe(chip);
    await fireEvent.click(chip);
    expect(onopen).toHaveBeenCalledOnce();
  });

  it("names the worst problem with how many more, in its colour, and describes every one", () => {
    render(HealthChip, { health: wrong, onopen: vi.fn() });
    const chip = screen.getByRole("button", { name: "Health: arm_joints 12 lost +2" });
    expect(chip.classList.contains("danger")).toBe(true);
    expect(chip.textContent).toContain("+2");
    const every = "arm_joints 12 lost; gripper silent; Disk 4 % free. Opens where it is shown in full.";
    expect(screen.getByRole("button", { description: every })).toBe(chip);
    // Cut short by its fixed width, it is said whole on hover.
    expect(chip.title).toBe(every);
  });

  it("is reached from the keyboard like any button", () => {
    render(HealthChip, { health: allWell, onopen: vi.fn() });
    const chip = screen.getByRole("button", { name: "Health: All well" });
    chip.focus();
    expect(document.activeElement).toBe(chip);
    expect(chip.tabIndex).toBe(0);
  });
});
