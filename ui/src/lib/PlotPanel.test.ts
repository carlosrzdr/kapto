import { render, screen } from "@testing-library/svelte";
import { describe, expect, it } from "vitest";

import PlotPanel from "./PlotPanel.svelte";
import { type PlotStream, SampleBuffer } from "./viewer/plots";

const arm: PlotStream = {
  name: "arm_joints",
  index: 0,
  fields: [
    { name: "joint_positions", values: [0, 1], names: ["joint_positions[0]", "joint_positions[1]"] },
    { name: "gripper", values: [2], names: ["gripper"] },
  ],
  samples: new SampleBuffer(3),
};

describe("PlotPanel", () => {
  it("names each value it draws, a role by the role it plays", () => {
    render(PlotPanel, {
      fields: ["arm_joints.joint_positions", "role:gripper.opening"],
      roles: { "gripper.opening": "arm_joints.gripper" },
      streams: [arm],
    });
    const plot = screen.getByRole("region", { name: "Plot of arm_joints.joint_positions, role:gripper.opening" });
    expect(Array.from(plot.querySelectorAll("li:not(.heading)"), (item) => item.textContent)).toEqual([
      "joint_positions[0]",
      "joint_positions[1]",
      "gripper.opening",
    ]);
    expect(screen.queryByText(/Waiting for/)).toBeNull();
  });

  it("says once what the roles it draws share", () => {
    render(PlotPanel, {
      fields: ["role:joints.joint_1.position", "role:joints.joint_2.position"],
      roles: { "joints.joint_1.position": "arm_joints.joint_positions[0]", "joints.joint_2.position": "arm_joints.joint_positions[1]" },
      streams: [arm],
    });
    const items = Array.from(document.querySelectorAll("li"), (item) => item.textContent);
    expect(items).toEqual(["joints · position", "joint_1", "joint_2"]);
  });

  it("says what it waits for until a stream it draws has opened", () => {
    render(PlotPanel, { fields: ["role:gripper.opening"], roles: { "gripper.opening": "arm_joints.gripper" }, streams: [] });
    expect(screen.getByText("Waiting for role:gripper.opening")).toBeTruthy();
  });
});
