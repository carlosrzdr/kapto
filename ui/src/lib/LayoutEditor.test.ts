import { fireEvent, render, screen } from "@testing-library/svelte";
import { describe, expect, it, vi } from "vitest";

import type { LayoutInput } from "./api-types";
import { chooseValue } from "./controls/testing";
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
  it("leaves moving, resizing and removing to the view, saying so", () => {
    editor();
    expect(screen.getByText(/Drag a panel by its grip, or an edge between two, on the view/)).toBeTruthy();
    expect(screen.queryAllByRole("button", { name: /^(Move|Remove) / })).toHaveLength(0);
  });

  it("adds a plot of the fields and roles picked, to the column chosen", async () => {
    const { onchange } = editor({ recordedOnly: ["lidar"] });
    const add = screen.getByRole("button", { name: "Add" });
    expect((add as HTMLButtonElement).disabled).toBe(true);
    await fireEvent.click(screen.getByRole("checkbox", { name: "arm_joints.joint_positions" }));
    await chooseValue("In", "2");
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
    await chooseValue("Shows", "camera");
    await chooseValue("Camera", "wrist_camera");
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
