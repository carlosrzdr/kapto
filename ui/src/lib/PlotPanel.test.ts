import { fireEvent, render, screen } from "@testing-library/svelte";
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

  it("says once, in its key, the stream it draws and what the roles share", () => {
    render(PlotPanel, {
      fields: ["role:joints.joint_1.position", "role:joints.joint_2.position"],
      roles: { "joints.joint_1.position": "arm_joints.joint_positions[0]", "joints.joint_2.position": "arm_joints.joint_positions[1]" },
      streams: [arm],
    });
    const items = Array.from(document.querySelectorAll("li"), (item) => item.textContent?.trim());
    expect(items).toEqual(["arm_joints · joints · position", "joint_1", "joint_2"]);
    // Two values show their ranges anyway: nothing to pick.
    expect(screen.queryAllByRole("button")).toHaveLength(0);
  });

  it("past two values, shows a value's range only once its entry in the key is picked", async () => {
    render(PlotPanel, {
      fields: ["arm_joints.joint_positions", "arm_joints.gripper"],
      roles: {},
      streams: [arm],
    });
    const entry = screen.getByRole("button", { name: "gripper" });
    expect(entry.getAttribute("aria-pressed")).toBe("false");
    await fireEvent.click(entry);
    expect(entry.getAttribute("aria-pressed")).toBe("true");
    await fireEvent.click(entry);
    expect(entry.getAttribute("aria-pressed")).toBe("false");
  });

  it("draws each value of its key in its own colour, the fifth dashed rather than in a colour again", () => {
    const wide: PlotStream = {
      name: "wide",
      index: 1,
      fields: [{ name: "v", values: [0, 1, 2, 3, 4], names: ["v[0]", "v[1]", "v[2]", "v[3]", "v[4]"] }],
      samples: new SampleBuffer(5),
    };
    render(PlotPanel, { fields: ["wide.v"], roles: {}, streams: [wide] });
    const swatches = screen.getAllByRole("button").map((entry) => entry.className.match(/series-\d dash-\d/)?.[0]);
    expect(swatches).toEqual(["series-0 dash-0", "series-1 dash-0", "series-2 dash-0", "series-3 dash-0", "series-0 dash-1"]);
  });

  it("says what it waits for until a stream it draws has opened", () => {
    render(PlotPanel, { fields: ["role:gripper.opening"], roles: { "gripper.opening": "arm_joints.gripper" }, streams: [] });
    expect(screen.getByText("Waiting for role:gripper.opening")).toBeTruthy();
  });
});
