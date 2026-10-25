import { fireEvent, render, screen } from "@testing-library/svelte";
import { afterEach, describe, expect, it, vi } from "vitest";

import PlotPanel from "./PlotPanel.svelte";
import { type PlotStream, SampleBuffer } from "./viewer/plots";

/** Each chart made, standing in for uPlot, which jsdom can't draw: what it was made with and shows. */
const charts = vi.hoisted(() => ({
  made: [] as { series: { label?: string; show: boolean }[]; destroyed: boolean }[],
}));
vi.mock("uplot", () => ({
  default: class {
    series: { label?: string; show: boolean }[];
    destroyed = false;
    cursor = {};
    bbox = { width: 300 };
    over = document.createElement("div");
    constructor(options: { series: { label?: string; show?: boolean }[] }) {
      this.series = options.series.map((series) => ({ label: series.label, show: series.show ?? true }));
      charts.made.push(this);
    }
    setSeries(index: number, { show }: { show: boolean }) {
      this.series[index]!.show = show;
    }
    batch(change: () => void) {
      change();
    }
    setData() {}
    setSize() {}
    valToPos() {
      return 0;
    }
    destroy() {
      this.destroyed = true;
    }
  },
}));

const arm: PlotStream = {
  name: "arm_joints",
  index: 0,
  fields: [
    { name: "joint_positions", values: [0, 1], names: ["joint_positions[0]", "joint_positions[1]"] },
    { name: "gripper", values: [2], names: ["gripper"] },
  ],
  samples: new SampleBuffer(3),
};

/** The labels of the series the chart shows. */
const showing = (chart: (typeof charts.made)[number]) => chart.series.filter((series, index) => index > 0 && series.show).map((series) => series.label);

afterEach(() => {
  charts.made = [];
  vi.restoreAllMocks();
});

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
  });

  it("has nothing to click in the key of a lone value, always shown", () => {
    render(PlotPanel, { fields: ["arm_joints.gripper"], roles: {}, streams: [arm] });
    expect(screen.queryAllByRole("button")).toHaveLength(0);
  });

  it("shows a value alone once its entry in the key is clicked, then every one again", async () => {
    render(PlotPanel, { fields: ["arm_joints.joint_positions", "arm_joints.gripper"], roles: {}, streams: [arm] });
    const entry = screen.getByRole("button", { name: "gripper" });
    const other = screen.getByRole("button", { name: "joint_positions[0]" });
    expect(entry.getAttribute("aria-pressed")).toBe("true");
    await fireEvent.click(entry);
    expect(entry.getAttribute("aria-pressed")).toBe("true");
    expect(other.getAttribute("aria-pressed")).toBe("false");
    expect(other.classList.contains("hidden")).toBe(true);
    // With a modifier, another beside it.
    await fireEvent.click(other, { ctrlKey: true });
    expect(other.getAttribute("aria-pressed")).toBe("true");
    await fireEvent.click(other);
    expect(entry.getAttribute("aria-pressed")).toBe("false");
    await fireEvent.click(other);
    expect(entry.getAttribute("aria-pressed")).toBe("true");
  });

  it("shows and hides values and their bands on the charts it made, never making them again", async () => {
    render(PlotPanel, { fields: ["arm_joints.joint_positions", "arm_joints.gripper"], roles: {}, streams: [arm] });
    await vi.waitFor(() => expect(charts.made).toHaveLength(1));
    const chart = charts.made[0]!;
    // Three values: each a line, no band.
    expect(showing(chart)).toEqual(["joint_positions[0]", "joint_positions[1]", "gripper"]);
    await fireEvent.click(screen.getByRole("button", { name: "gripper" }));
    // Alone, its band from minimum to maximum.
    await vi.waitFor(() => expect(showing(chart)).toEqual(["gripper min", "gripper max"]));
    await fireEvent.click(screen.getByRole("button", { name: "joint_positions[1]" }), { shiftKey: true });
    await vi.waitFor(() => expect(showing(chart)).toEqual(["joint_positions[1] min", "joint_positions[1] max", "gripper min", "gripper max"]));
    await fireEvent.click(screen.getByRole("button", { name: "joint_positions[0]" }), { metaKey: true });
    await vi.waitFor(() => expect(showing(chart)).toEqual(["joint_positions[0]", "joint_positions[1]", "gripper"]));
    expect(charts.made).toHaveLength(1);
    expect(chart.destroyed).toBe(false);
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
