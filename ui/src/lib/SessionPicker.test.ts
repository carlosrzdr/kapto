import { fireEvent, render, screen } from "@testing-library/svelte";
import { describe, expect, it, vi } from "vitest";

import type { ProfilesOverview, TasksOverview } from "./api-types";
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
  it("offers every profile, those for another robot greyed with the package they need", () => {
    picker();
    const profile = screen.getByRole("combobox", { name: "Profile" });
    const options = [...profile.querySelectorAll("option")].map((option) => [option.textContent?.trim(), option.disabled]);
    expect(options).toEqual([
      ["None", false],
      ["cobot_day (needs cobot)", true],
      ["lab_ana · Ana", false],
    ]);
    const task = screen.getByRole("combobox", { name: "Task" });
    expect([...task.querySelectorAll("option")].map((option) => option.textContent?.trim())).toEqual(["None", "Pick and place"]);
  });

  it("says what was chosen, and asks for the lists again when one is opened", async () => {
    const { onchange, onopen } = picker();
    const profile = screen.getByRole("combobox", { name: "Profile" });
    await fireEvent.focus(profile);
    expect(onopen).toHaveBeenCalled();
    await fireEvent.change(profile, { target: { value: "lab_ana" } });
    expect(onchange).toHaveBeenLastCalledWith({ profile: "lab_ana", task: null });
    await fireEvent.change(screen.getByRole("combobox", { name: "Task" }), { target: { value: "pick_and_place" } });
    expect(onchange).toHaveBeenLastCalledWith({ profile: null, task: "pick_and_place" });
  });

  it("can't be changed while an episode is recorded", () => {
    picker({ disabled: true });
    expect(screen.getByRole("combobox", { name: "Profile" }).hasAttribute("disabled")).toBe(true);
    expect(screen.getByRole("combobox", { name: "Task" }).hasAttribute("disabled")).toBe(true);
  });
});
