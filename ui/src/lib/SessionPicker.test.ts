import { fireEvent, render, screen } from "@testing-library/svelte";
import { describe, expect, it, vi } from "vitest";

import type { ProfilesOverview, TasksOverview } from "./api-types";
import { choose, offered, openList } from "./controls/testing";
import SessionPicker from "./SessionPicker.svelte";

const profiles: ProfilesOverview = {
  package: "arm7",
  profiles: [
    { id: "cobot_day", package: "cobot", available: false, tags: [] },
    { id: "lab_ana", package: "arm7", available: true, operator: "Ana", mode: "teleoperated", tags: [] },
  ],
  problems: [],
};

const tasks: TasksOverview = {
  tasks: [{ id: "pick_and_place", name: "Pick and place", objects: [], subtasks: ["reach", "grasp"] }],
  problems: [],
};

function picker(overrides: { disabled?: boolean } = {}) {
  const onchange = vi.fn();
  const onopen = vi.fn();
  render(SessionPicker, {
    profiles,
    tasks,
    choice: { profile: null, task: null },
    disabled: overrides.disabled ?? false,
    onchange,
    onopen,
  });
  return { onchange, onopen };
}

describe("SessionPicker", () => {
  it("offers every profile, those for another robot unavailable with the package they need", async () => {
    picker();
    expect(offered(await openList("Profile"))).toEqual([
      ["None", true],
      ["cobot_day Needs the package cobot", false],
      ["lab_ana Ana", true],
    ]);
    await fireEvent.keyDown(document.activeElement ?? document.body, { key: "Escape" });
    expect(offered(await openList("Task"))).toEqual([
      ["None", true],
      ["Pick and place pick_and_place", true],
    ]);
  });

  it("says what was chosen, and asks for the lists again when one is opened", async () => {
    const { onchange, onopen } = picker();
    await choose("Profile", "lab_ana");
    expect(onopen).toHaveBeenCalled();
    expect(onchange).toHaveBeenLastCalledWith({ profile: "lab_ana", task: null });
    await choose("Task", "Pick and place");
    expect(onchange).toHaveBeenLastCalledWith({ profile: null, task: "pick_and_place" });
  });

  it("can't be changed while an episode is recorded", () => {
    picker({ disabled: true });
    expect(screen.getByRole("combobox", { name: "Profile" }).hasAttribute("disabled")).toBe(true);
    expect(screen.getByRole("combobox", { name: "Task" }).hasAttribute("disabled")).toBe(true);
  });
});
