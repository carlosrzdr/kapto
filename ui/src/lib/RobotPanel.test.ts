import { render, screen } from "@testing-library/svelte";
import { describe, expect, it } from "vitest";

import type { RobotOverview } from "./api-types";
import RobotPanel from "./RobotPanel.svelte";

const robot: RobotOverview = {
  name: "arm7",
  files: { urdf: "packages/arm7/urdf/arm7.urdf", streams: "streams.toml", mapping: "mapping.toml" },
  links: 11,
  joints: [
    { name: "joint_1", kind: "revolute", lower: -Math.PI, upper: Math.PI, position_from: "arm_joints.joint_positions[0]" },
    { name: "tool_mount", kind: "fixed" },
    { name: "gripper", kind: "prismatic", lower: 0, upper: 0.04 },
    { name: "gripper_mirror", kind: "prismatic", lower: 0, upper: 0.04, mimics: "gripper" },
  ],
  streams: [
    {
      name: "arm_joints",
      input: "udp 0.0.0.0:9000",
      rate_hz: 2000,
      kept: false,
      sequence: true,
      decoder: "struct",
      fields: [
        { name: "joint_positions", ty: "f64", count: 7, unit: "rad" },
        { name: "gripper", ty: "f64", count: 1, unit: "m" },
      ],
    },
  ],
  warnings: [
    {
      code: "mapping.joint_unmapped",
      message: "`gripper` has no position mapped",
      remedy: "Map it in mapping.toml.",
    },
  ],
};

describe("RobotPanel", () => {
  it("lists the moving joints with their range and what drives each", () => {
    render(RobotPanel, { robot });
    expect(screen.getByText(/3 moving joints/)).toBeTruthy();
    expect(screen.queryByRole("row", { name: /tool_mount/ })).toBeNull();
    const first = screen.getByRole("row", { name: /^joint_1/ });
    expect(first.textContent).toContain("-180 … 180°");
    expect(first.textContent).toContain("arm_joints.joint_positions[0]");
    expect(screen.getByRole("row", { name: /^gripper not mapped/ }).textContent).toContain("0 … 40 mm");
    expect(screen.getByRole("row", { name: /^gripper_mirror/ }).textContent).toContain("follows gripper");
  });

  it("lists each stream with its input, rate and decoded fields", () => {
    render(RobotPanel, { robot });
    const stream = screen.getByRole("row", { name: /^arm_joints/ });
    expect(stream.textContent).toContain("udp 0.0.0.0:9000");
    expect(stream.textContent).toContain("2000 Hz");
    expect(stream.textContent).toContain("struct: joint_positions[7] rad, gripper m");
  });

  it("shows problems that don't stop Kapto, with what to do", () => {
    render(RobotPanel, { robot });
    expect(screen.getByText(/has no position mapped/).textContent).toContain("Map it in mapping.toml.");
  });

  it("shows where each mapped joint is now, and where that is in its range", () => {
    // joint_positions[0..7], then gripper.
    const values = [{ stream: "arm_joints", values: [Math.PI / 2, 0, 0, 0, 0, 0, 0, 0.04], age_ms: 12 }];
    render(RobotPanel, { robot, values });
    const first = screen.getByRole("row", { name: /^joint_1/ });
    expect(first.textContent).toContain("90.0°");
    expect(first.querySelector("meter")?.value).toBe(0.75);
    // Not mapped, so not known.
    expect(screen.getByRole("row", { name: /^gripper / }).querySelectorAll("td")[0]?.textContent?.trim()).toBe("–");
  });

  it("shows nothing for values that have gone stale", () => {
    const values = [{ stream: "arm_joints", values: [1, 0, 0, 0, 0, 0, 0, 0], age_ms: 5000 }];
    render(RobotPanel, { robot, values });
    expect(screen.getByRole("row", { name: /^joint_1/ }).querySelectorAll("td")[0]?.textContent?.trim()).toBe("–");
  });
});
