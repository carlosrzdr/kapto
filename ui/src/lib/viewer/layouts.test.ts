import { describe, expect, it } from "vitest";

import type { LayoutInput } from "../api-types";
import {
  added,
  apportioned,
  browserLayouts,
  editable,
  laidOut,
  MAX_PANEL_TITLE,
  MAX_PANELS,
  moved,
  nudged,
  panelCount,
  placed,
  panelTitle,
  removed,
  reshared,
  retitled,
  withHeights,
  withWidths,
} from "./layouts";
import { dragged } from "./dragging";

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

  it("drops a dragged panel before any panel of any column, or as a new column between two", () => {
    // Down its own column: before the panel it was dropped on, past itself.
    expect(kinds(placed(layout, 1, 0, { column: 1, at: 2 }))).toEqual([["robot"], ["plot", "camera"]]);
    expect(kinds(placed(layout, 1, 1, { column: 1, at: 0 }))).toEqual([["robot"], ["plot", "camera"]]);
    // Into another column, at the top; the one it leaves empty goes.
    expect(kinds(placed(layout, 0, 0, { column: 1, at: 0 }))).toEqual([["robot", "camera", "plot"]]);
    // A new column on the left, as wide as the narrowest beside it.
    const apart = placed(layout, 1, 1, { between: 0 });
    expect(kinds(apart)).toEqual([["plot"], ["robot"], ["camera"]]);
    expect(apart.columns.map((column) => column.width)).toEqual([3, 3, 2]);
    expect(kinds(placed(layout, 1, 0, { between: 2 }))).toEqual([["robot"], ["plot"], ["camera"]]);
    // Beside the column it is alone in, or on itself: nothing moves.
    expect(placed(layout, 0, 0, { between: 1 })).toBe(layout);
    expect(kinds(placed(layout, 1, 0, { column: 1, at: 1 }))).toEqual(kinds(layout));
    expect(placed(layout, 5, 0, { column: 0, at: 0 })).toBe(layout);
  });

  it("turns sizes dragged on the page into shares of 1 to 12 in the same proportions", () => {
    expect(apportioned([600, 300])).toEqual([12, 6]);
    expect(apportioned([900, 300, 10])).toEqual([12, 4, 1]);
    expect(withWidths(layout, [5, 7]).columns.map((column) => column.width)).toEqual([5, 7]);
    expect(withHeights(layout, 1, [2]).columns[1]!.panels.map((panel) => panel.height)).toEqual([2, 1]);
  });

  it("turns a column's panels' sizes into shares of the pixels a share had, in a column that scrolls or one with room", () => {
    // More than fit, each at its least: 192 pixels a share. The edge after
    // the first dragged down a share and a half gives the first two, the
    // second kept at its least; dragged back up past it, the second grows.
    const scrolls = [192, 192, 192];
    expect(reshared(scrolls, [1, 1, 1])).toEqual([1, 1, 1]);
    expect(reshared(dragged(scrolls, 0, 288), [1, 1, 1])).toEqual([2, 1, 1]);
    expect(reshared(dragged([384, 192, 192], 0, -230), [2, 1, 1])).toEqual([1, 2, 1]);
    // With room, grown past their least: measured, they keep their shares.
    expect(reshared([600, 300], [2, 1])).toEqual([2, 1]);
    expect(reshared(dragged([600, 300], 0, -300), [2, 1])).toEqual([1, 2]);
  });

  it("moves an edge a share by the keyboard as far as both sides can go, even between two at their least", () => {
    expect(nudged([2, 2], 0, 1)).toEqual([3, 1]);
    expect(nudged([1, 1, 1], 1, 1)).toEqual([1, 2, 1]);
    expect(nudged([1, 1], 0, -1)).toEqual([1, 2]);
    expect(nudged([12, 1], 0, 1)).toBeNull();
    expect(nudged([1, 1], 1, 1)).toBeNull();
  });

  it("gives a panel a title of its own, on one line and at most as long as layout.toml allows, and takes it away when blank", () => {
    const titled = retitled(layout, 1, 1, "  Gripper  ");
    expect(titled.columns[1]!.panels[1]!.title).toBe("Gripper");
    expect(titled.columns[1]!.panels[0]!.title).toBeUndefined();
    expect("title" in retitled(titled, 1, 1, "   ").columns[1]!.panels[1]!).toBe(false);
    expect(retitled(layout, 4, 0, "Nowhere")).toBe(layout);
    expect(panelTitle("Wrist\nforce")).toBe("Wrist force");
    // Counted in characters, as the server counts them.
    expect(panelTitle("é".repeat(MAX_PANEL_TITLE + 5))).toBe("é".repeat(MAX_PANEL_TITLE));
    expect(panelTitle("🦾".repeat(MAX_PANEL_TITLE + 1))).toHaveLength(2 * MAX_PANEL_TITLE);
    expect(panelTitle("")).toBeNull();
  });

  it("never changes the layout it is given", () => {
    const before = structuredClone(layout);
    placed(layout, 1, 0, { between: 0 });
    withWidths(layout, [9, 9]);
    moved(layout, 1, 0, "left");
    removed(layout, 0, 0);
    withHeights(layout, 0, [4]);
    retitled(layout, 0, 0, "Arm");
    expect(layout).toEqual(before);
  });
});

describe("the operator's own layout", () => {
  it("is laid out as the view draws it until the server says what it lacks, and edited without that", () => {
    const view = laidOut(layout);
    expect(view.source).toBe("own");
    expect(view.columns.flatMap((column) => column.panels.map((panel) => panel.missing))).toEqual([[], [], []]);
    const checked = {
      ...view,
      columns: view.columns.map((column) => ({
        ...column,
        panels: column.panels.map((panel) => ({ ...panel, missing: ["no stream `wrist_camera`"] })),
      })),
    };
    expect(editable(checked)).toEqual(layout);
    // A panel's own title kept both ways, the server's `null` for none left out.
    const titled = retitled(layout, 0, 0, "Arm");
    expect(laidOut(titled).columns.map((column) => column.panels.map((panel) => panel.title))).toEqual([["Arm"], [null, null]]);
    expect(editable(laidOut(titled))).toEqual(titled);
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
    const held = laidOut(layout);
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
