import { fireEvent, render, screen } from "@testing-library/svelte";
import { describe, expect, it, vi } from "vitest";

import EnvironmentPanel from "./EnvironmentPanel.svelte";
import { placedShape } from "./viewer/environment";

describe("EnvironmentPanel", () => {
  it("adds a shape, then places the one chosen", async () => {
    const onput = vi.fn();
    const { rerender } = render(EnvironmentPanel, { placed: [], onput, onremove: vi.fn() });
    await fireEvent.click(screen.getByRole("button", { name: "Add box" }));
    const added = onput.mock.calls[0]?.[0];
    expect(added).toMatchObject({ shape: "box", name: "Box", position: [0.6, 0, 0.1] });
    await rerender({ placed: [added], onput, onremove: vi.fn() });
    await fireEvent.change(screen.getByRole("spinbutton", { name: "position z" }), { target: { value: "-0.02" } });
    expect(onput.mock.calls[1]?.[0]).toMatchObject({ id: added.id, position: [0.6, 0, -0.02] });
    // A size can't be zero or less: it would vanish.
    await fireEvent.change(screen.getByRole("spinbutton", { name: "scale x" }), { target: { value: "0" } });
    expect(onput.mock.calls[2]?.[0].scale[0]).toBe(0.001);
  });

  it("removes the one chosen", async () => {
    const onremove = vi.fn();
    render(EnvironmentPanel, { placed: [placedShape("cylinder", "c1")], onput: vi.fn(), onremove });
    await fireEvent.click(screen.getByRole("button", { name: "Cylinder" }));
    await fireEvent.click(screen.getByRole("button", { name: "Remove" }));
    expect(onremove).toHaveBeenCalledWith("c1");
  });

  it("refuses a file it can't load, saying which it can", async () => {
    const onput = vi.fn();
    render(EnvironmentPanel, { placed: [], onput, onremove: vi.fn() });
    const input = screen.getByLabelText("Load a mesh") as HTMLInputElement;
    await fireEvent.change(input, { target: { files: [new File(["x"], "cell.dae")] } });
    expect(screen.getByRole("alert").textContent).toContain("cell.dae isn't a mesh this view loads");
    expect(onput).not.toHaveBeenCalled();
  });
});

describe("EnvironmentPanel's names", () => {
  it("numbers a shape added again, so each can be told apart", async () => {
    const onput = vi.fn();
    render(EnvironmentPanel, { placed: [placedShape("box", "b1")], onput, onremove: vi.fn() });
    await fireEvent.click(screen.getByRole("button", { name: "Add box" }));
    expect(onput.mock.calls[0]?.[0].name).toBe("Box 2");
  });

  it("adds a sample cell to start from: a table under the robot, a cube to pick and a bin, each its own", async () => {
    const onput = vi.fn();
    render(EnvironmentPanel, { placed: [], onput, onremove: vi.fn() });
    await fireEvent.click(screen.getByRole("button", { name: "Add a sample cell" }));
    const added = onput.mock.calls.map(([placed]) => placed);
    expect(added.map((placed) => placed.name)).toEqual(["Table", "Red cube", "Bin"]);
    expect(new Set(added.map((placed) => placed.id)).size).toBe(3);
    // The table's top is just below the robot's base.
    const table = added[0];
    expect(table.position[2] + table.scale[2] / 2).toBeCloseTo(0);
  });
});
