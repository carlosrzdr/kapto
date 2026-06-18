import { fireEvent, render, screen } from "@testing-library/svelte";
import { describe, expect, it, vi } from "vitest";

import type { LayoutInput } from "./api-types";
import LayoutEditor from "./LayoutEditor.svelte";

const layout: LayoutInput = {
  columns: [
    { width: 3, panels: [{ kind: "robot", height: 1, fields: [] }] },
    { width: 2, panels: [{ kind: "plot", height: 1, fields: ["role:gripper.opening"] }] },
  ],
};
const offered = { cameras: ["wrist_camera"], fields: ["arm_joints.joint_positions"], roles: ["gripper.opening"] };

function editor(more: object = {}) {
  const onchange = vi.fn<(next: LayoutInput) => void>();
  const onsave = vi.fn();
  const onreset = vi.fn();
  render(LayoutEditor, { layout, offered, onchange, onsave, onreset, ...more });
  return { onchange, onsave, onreset };
}

const kinds = (next: LayoutInput) => next.columns.map((column) => column.panels.map((panel) => panel.kind));

describe("LayoutEditor", () => {
  it("moves, resizes and removes each panel, and widens each column", async () => {
    const { onchange } = editor();
    await fireEvent.click(screen.getByRole("button", { name: "Move Plot of role:gripper.opening to the column on the left" }));
    expect(kinds(onchange.mock.lastCall![0])).toEqual([["robot", "plot"]]);
    await fireEvent.click(screen.getByRole("button", { name: "Robot taller" }));
    expect(onchange.mock.lastCall![0].columns[0]!.panels[0]!.height).toBe(2);
    await fireEvent.click(screen.getByRole("button", { name: "Column 2 wider" }));
    expect(onchange.mock.lastCall![0].columns[1]!.width).toBe(3);
    await fireEvent.click(screen.getByRole("button", { name: "Remove Robot" }));
    expect(kinds(onchange.mock.lastCall![0])).toEqual([["plot"]]);
  });

  it("adds a plot of the fields and roles picked, to the column chosen", async () => {
    const { onchange } = editor({ recordedOnly: ["lidar"] });
    const add = screen.getByRole("button", { name: "Add" });
    expect((add as HTMLButtonElement).disabled).toBe(true);
    await fireEvent.click(screen.getByRole("checkbox", { name: "arm_joints.joint_positions" }));
    await fireEvent.change(screen.getByRole("combobox", { name: "In" }), { target: { value: "2" } });
    await fireEvent.click(add);
    expect(onchange.mock.lastCall![0].columns[2]).toEqual({
      width: 1,
      panels: [{ kind: "plot", height: 1, fields: ["arm_joints.joint_positions"] }],
    });
    // A stream without a decoder has nothing to offer.
    expect(screen.getByText("lidar: recorded only, nothing decoded to plot")).toBeTruthy();
  });

  it("adds a camera the package has", async () => {
    const { onchange } = editor();
    await fireEvent.change(screen.getByRole("combobox", { name: "Shows" }), { target: { value: "camera" } });
    await fireEvent.change(screen.getByRole("combobox", { name: "Camera" }), { target: { value: "wrist_camera" } });
    await fireEvent.click(screen.getByRole("button", { name: "Add" }));
    expect(onchange.mock.lastCall![0].columns[0]!.panels[1]).toEqual({ kind: "camera", height: 1, stream: "wrist_camera", fields: [] });
  });

  it("saves as default and resets, and says why one failed", async () => {
    const { onsave, onreset } = editor({ said: "No robot package to save the layout for.", failed: true });
    await fireEvent.click(screen.getByRole("button", { name: "Save as default" }));
    await fireEvent.click(screen.getByRole("button", { name: "Reset to the package's" }));
    expect([onsave.mock.calls.length, onreset.mock.calls.length]).toEqual([1, 1]);
    expect(screen.getByRole("alert").textContent).toBe("No robot package to save the layout for.");
  });
});
