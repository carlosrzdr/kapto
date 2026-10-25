import { fireEvent, render, screen } from "@testing-library/svelte";
import { describe, expect, it, vi } from "vitest";

import PanelTitle from "./PanelTitle.svelte";
import { MAX_PANEL_TITLE } from "./viewer/layouts";

const shown = (title: string | null) => {
  const onsave = vi.fn();
  const oncancel = vi.fn();
  render(PanelTitle, { title, made: "arm_joints.gripper", label: "Title of the plot", onsave, oncancel });
  return { field: screen.getByRole("textbox", { name: "Title of the plot" }) as HTMLInputElement, onsave, oncancel };
};

describe("PanelTitle", () => {
  it("starts from the panel's own title, selected and focused, the made one shown while it is empty", () => {
    const { field } = shown("Gripper");
    expect(field.value).toBe("Gripper");
    expect(document.activeElement).toBe(field);
    expect([field.selectionStart, field.selectionEnd]).toEqual([0, 7]);
    expect(field.placeholder).toBe("arm_joints.gripper");
    expect(field.maxLength).toBe(MAX_PANEL_TITLE);
  });

  it("saves what is written on Enter, once, even as the field is left after", async () => {
    const { field, onsave, oncancel } = shown(null);
    await fireEvent.input(field, { target: { value: "Wrist force" } });
    await fireEvent.keyDown(field, { key: "Enter" });
    await fireEvent.blur(field);
    expect(onsave.mock.calls).toEqual([["Wrist force", true]]);
    expect(oncancel).not.toHaveBeenCalled();
  });

  it("saves what is written when the field is left, an empty one to clear the title", async () => {
    const { field, onsave } = shown("Gripper");
    await fireEvent.input(field, { target: { value: "" } });
    await fireEvent.blur(field);
    expect(onsave.mock.calls).toEqual([["", false]]);
  });

  it("leaves the title as it was on Escape, without the page seeing the key", async () => {
    const { field, onsave, oncancel } = shown("Gripper");
    const page = vi.fn();
    window.addEventListener("keydown", page);
    await fireEvent.input(field, { target: { value: "Something else" } });
    await fireEvent.keyDown(field, { key: "Escape" });
    await fireEvent.blur(field);
    window.removeEventListener("keydown", page);
    expect(oncancel).toHaveBeenCalledTimes(1);
    expect(onsave).not.toHaveBeenCalled();
    expect(page).not.toHaveBeenCalled();
  });
});
