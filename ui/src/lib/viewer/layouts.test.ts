import { describe, expect, it } from "vitest";

import type { LayoutInput } from "../api-types";
import {
  added,
  apportioned,
  editable,
  keysAfter,
  keysFor,
  laidOut,
  MAX_PANEL_TITLE,
  MAX_PANELS,
  MAX_ROBOT_VIEWS,
  moved,
  NEW_COLUMN,
  nudged,
  panelCount,
  placed,
  panelTitle,
  removed,
  reshared,
  retitled,
  robotCount,
  showing,
  type ShowingColumn,
  withHeights,
  withView,
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

  it("adds at most four robot panels, as many as the page draws each frame", () => {
    let views = layout;
    for (let index = 0; index < MAX_ROBOT_VIEWS + 2; index++) {
      views = added(views, 0, { kind: "robot", height: 1, fields: [], view: "side" });
    }
    expect(robotCount(views)).toBe(MAX_ROBOT_VIEWS);
    // Other panels still go in.
    expect(panelCount(added(views, 1, { kind: "health", height: 1, fields: [] }))).toBe(panelCount(views) + 1);
  });

  it("sees a robot panel from a preset or a place of its own, and only a robot panel", () => {
    const placedView = { from: [1.5, -1, 0.8], at: [0, 0, 0.4] };
    const seen = withView(layout, 0, 0, placedView);
    expect(seen.columns[0]!.panels[0]!.view).toEqual(placedView);
    expect(withView(seen, 0, 0, "top").columns[0]!.panels[0]!.view).toBe("top");
    expect("view" in withView(seen, 0, 0, null).columns[0]!.panels[0]!).toBe(false);
    // Not a robot panel, or not there: unchanged.
    expect(withView(layout, 1, 0, "top")).toBe(layout);
    expect(withView(layout, 3, 0, "top")).toBe(layout);
    // Kept both ways, the server's `null` for none left out.
    expect(editable(laidOut(seen))).toEqual(seen);
    expect(laidOut(layout).columns[0]!.panels[0]!.view).toBeNull();
  });

  it("never changes the layout it is given", () => {
    const before = structuredClone(layout);
    placed(layout, 1, 0, { between: 0 });
    withWidths(layout, [9, 9]);
    moved(layout, 1, 0, "left");
    removed(layout, 0, 0);
    withHeights(layout, 0, [4]);
    retitled(layout, 0, 0, "Arm");
    withView(layout, 0, 0, { from: [1, 0, 1], at: [0, 0, 0] });
    expect(layout).toEqual(before);
  });
});

describe("the view while a panel is carried", () => {
  const keys = keysFor(layout, null);
  /** Each column shown as its panels' kinds, the one carried as its shadow. */
  const seen = (columns: ShowingColumn<{ kind: string }>[]) =>
    columns.map((column) => column.panels.map(({ panel, carried }) => (carried ? `(${panel.kind})` : panel.kind)));

  it("shows the panels as they are, by keys that follow the layout's shape", () => {
    expect(seen(showing(layout.columns, keys, null))).toEqual([["robot"], ["camera", "plot"]]);
    expect(keys.columns.map((column) => column.panels)).toEqual([["p0.0"], ["p1.0", "p1.1"]]);
    // The same shape keeps the keys it had, with the layout's widths; another shape is keyed anew.
    const kept = { columns: [{ key: "a", width: 1, panels: ["x"] }, { key: "b", width: 1, panels: ["y", "z"] }] };
    expect(keysFor(withWidths(layout, [4, 5]), kept)).toEqual({ columns: [{ ...kept.columns[0], width: 4 }, { ...kept.columns[1], width: 5 }] });
    expect(keysFor(removed(layout, 1, 0), kept).columns.map((column) => column.panels)).toEqual([["p0.0"], ["p1.0"]]);
  });

  it("lays the others out as dropping it now would, the one carried in its place", () => {
    // Up its own column: the camera under it.
    expect(seen(showing(layout.columns, keys, { column: 1, at: 1, to: { column: 1, at: 0 } }))).toEqual([["robot"], ["(plot)", "camera"]]);
    // Into another column, which takes the room its own column leaves.
    expect(seen(showing(layout.columns, keys, { column: 0, at: 0, to: { column: 1, at: 1 } }))).toEqual([["camera", "(robot)", "plot"]]);
    // As a new column, as wide as the narrowest beside it, keyed as new.
    const apart = showing(layout.columns, keys, { column: 1, at: 0, to: { between: 0 } });
    expect(seen(apart)).toEqual([["(camera)"], ["robot"], ["plot"]]);
    expect(apart.map((column) => [column.key, column.width])).toEqual([[NEW_COLUMN, 3], ["c0", 3], ["c1", 2]]);
    // Not yet anywhere, or where it was: as they are.
    expect(seen(showing(layout.columns, keys, { column: 1, at: 0, to: null }))).toEqual([["robot"], ["(camera)", "plot"]]);
    expect(seen(showing(layout.columns, keys, { column: 0, at: 0, to: { between: 1 } }))).toEqual([["(robot)"], ["camera", "plot"]]);
  });

  it("arranges as the drop does, each panel keeping its key, a column made new given one", () => {
    for (const to of [{ column: 0, at: 0 }, { column: 1, at: 2 }, { between: 0 }, { between: 2 }] as const) {
      const dropped = placed(layout, 1, 0, to);
      const shownThen = showing(layout.columns, keys, { column: 1, at: 0, to });
      expect(seen(shownThen).map((column) => column.map((kind) => kind.replace(/[()]/g, "")))).toEqual(kinds(dropped));
      // After the drop, the keys follow the panels to where they were shown.
      let made = 0;
      const after = keysAfter(keys, (each) => placed(each, 1, 0, to), () => `n${(made += 1)}`);
      expect(showing(dropped.columns, keysFor(dropped, after), null).map((column) => column.panels.map((each) => each.key))).toEqual(
        shownThen.map((column) => column.panels.map((each) => each.key)),
      );
      expect(after.columns.every((column) => column.key !== NEW_COLUMN && column.key !== undefined)).toBe(true);
    }
  });

  it("keeps each panel's key as the keyboard moves it", () => {
    const after = keysAfter(keys, (each) => moved(each, 1, 1, "left"), () => "n1");
    expect(after.columns.map((column) => column.panels)).toEqual([["p0.0", "p1.1"], ["p1.0"]]);
    expect(keysAfter(keys, (each) => moved(each, 0, 0, "right"), () => "n1").columns.map((column) => column.key)).toEqual(["c1"]);
    expect(keysAfter(keys, (each) => moved(each, 1, 1, "right"), () => "n1").columns.map((column) => column.key)).toEqual(["c0", "c1", "n1"]);
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
