import { describe, expect, it } from "vitest";

import type { LayoutInput } from "../api-types";
import {
  added,
  browserLayouts,
  editable,
  heightened,
  lacking,
  MAX_PANELS,
  moved,
  type Offered,
  panelCount,
  removed,
  shown,
  widened,
} from "./layouts";

const offered: Offered = {
  cameras: ["wrist_camera"],
  fields: ["arm_joints.joint_positions", "arm_joints.gripper"],
  roles: ["gripper.opening", "joints.joint_1.position"],
};

/** The robot on the left; the camera and a plot on the right. */
const layout: LayoutInput = {
  columns: [
    { width: 3, panels: [{ kind: "robot", height: 1, fields: [] }] },
    {
      width: 2,
      panels: [
        { kind: "camera", height: 1, stream: "wrist_camera", fields: [] },
        { kind: "plot", height: 1, fields: ["role:gripper.opening"] },
      ],
    },
  ],
};

const kinds = (each: LayoutInput) => each.columns.map((column) => column.panels.map((panel) => panel.kind));

describe("editing a layout", () => {
  it("moves a panel up and down its column, and to the column beside it", () => {
    expect(kinds(moved(layout, 1, 1, "up"))).toEqual([["robot"], ["plot", "camera"]]);
    expect(kinds(moved(layout, 1, 0, "left"))).toEqual([["robot", "camera"], ["plot"]]);
    // Past the last column, it makes one; a column left empty goes.
    expect(kinds(moved(layout, 0, 0, "right"))).toEqual([["camera", "plot", "robot"]]);
    expect(kinds(moved(layout, 1, 1, "right"))).toEqual([["robot"], ["camera"], ["plot"]]);
    // Nowhere to go: unchanged.
    expect(moved(layout, 0, 0, "up")).toBe(layout);
    expect(moved(layout, 0, 0, "left")).toBe(layout);
  });

  it("adds a panel to a column or a new one, never past what a page can draw", () => {
    expect(kinds(added(layout, 0, { kind: "events", height: 1, fields: [] }))).toEqual([["robot", "events"], ["camera", "plot"]]);
    expect(kinds(added(layout, 2, { kind: "health", height: 1, fields: [] }))).toEqual([["robot"], ["camera", "plot"], ["health"]]);
    let full = layout;
    for (let index = 0; index < MAX_PANELS; index++) {
      full = added(full, 0, { kind: "events", height: 1, fields: [] });
    }
    expect(panelCount(full)).toBe(MAX_PANELS);
  });

  it("removes a panel, and its column once empty", () => {
    expect(kinds(removed(layout, 0, 0))).toEqual([["camera", "plot"]]);
  });

  it("resizes panels and columns from 1 to 12", () => {
    expect(heightened(layout, 1, 1, 2).columns[1]!.panels[1]!.height).toBe(3);
    expect(heightened(layout, 1, 1, -5).columns[1]!.panels[1]!.height).toBe(1);
    expect(widened(layout, 0, 20).columns[0]!.width).toBe(12);
  });

  it("never changes the layout it is given", () => {
    const before = structuredClone(layout);
    moved(layout, 1, 0, "left");
    removed(layout, 0, 0);
    heightened(layout, 0, 0, 1);
    expect(layout).toEqual(before);
  });
});

describe("what a layout lacks", () => {
  it("says what each panel names that the package doesn't have", () => {
    expect(lacking({ kind: "camera", height: 1, stream: "head_camera", fields: [] }, offered)).toEqual(["no camera `head_camera`"]);
    expect(
      lacking({ kind: "plot", height: 1, fields: ["role:gripper.force", "arm_joints.joint_positions[2]", "base.speed"] }, offered),
    ).toEqual(["no field plays `gripper.force`", "no plotted field `base.speed`"]);
    expect(lacking({ kind: "robot", height: 1, fields: [] }, offered)).toEqual([]);
  });

  it("is shown as the view draws it, and edited without what the API works out", () => {
    const view = shown(layout, { ...offered, cameras: [] });
    expect(view.source).toBe("own");
    expect(view.columns[1]!.panels[0]!.missing).toEqual(["no camera `wrist_camera`"]);
    expect(editable(view)).toEqual(layout);
  });
});

describe("keeping a layout in the browser", () => {
  it("keeps one per package, and forgets it", () => {
    const kept = new Map<string, string>();
    const storage = {
      getItem: (name: string) => kept.get(name) ?? null,
      setItem: (name: string, value: string) => void kept.set(name, value),
      removeItem: (name: string) => void kept.delete(name),
    };
    const store = browserLayouts(storage);
    store.save("arm7", layout);
    expect(store.load("arm7")).toEqual(layout);
    expect(store.load("rover")).toBeNull();
    store.clear("arm7");
    expect(store.load("arm7")).toBeNull();
  });

  it("does without when the browser keeps nothing, or kept something else", () => {
    const refusing = {
      getItem: () => {
        throw new Error("denied");
      },
      setItem: () => {
        throw new Error("denied");
      },
      removeItem: () => {},
    };
    const store = browserLayouts(refusing);
    expect(() => store.save("arm7", layout)).not.toThrow();
    expect(store.load("arm7")).toBeNull();
    const garbled = browserLayouts({ getItem: () => "{oops", setItem: () => {}, removeItem: () => {} });
    expect(garbled.load("arm7")).toBeNull();
  });
});

describe("a layout from the page's state", () => {
  it("is edited although the page holds it in proxies", () => {
    // As Svelte's state holds it: every object and array a proxy.
    const held = shown(layout, offered);
    const view = {
      ...held,
      columns: held.columns.map((column) => ({
        ...column,
        panels: column.panels.map((panel) => new Proxy({ ...panel, fields: new Proxy(panel.fields, {}) }, {})),
      })),
    };
    expect(kinds(removed(editable(view), 0, 0))).toEqual([["camera", "plot"]]);
  });
});
