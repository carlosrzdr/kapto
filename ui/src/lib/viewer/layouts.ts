// The operator's own layout: edited in the page and kept in this browser per
// robot package; the server says what each of its panels lacks.
// A layout kept here comes before the package's default and its own.

import type { LayoutInput, PanelInput, ViewLayout, ViewPanel } from "../api-types";

/** The most panels a page can draw, as `layout.toml` allows. */
export const MAX_PANELS = 24;
/** The most fields one plot draws. */
export const MAX_PLOT_FIELDS = 16;
/** The largest relative width or height. */
export const MAX_SHARE = 12;

/** What the package has that a layout may show. */
export interface Offered {
  /** Its camera streams, by name. */
  cameras: string[];
  /** Each decoded stream's plotted fields, as `stream.field`. */
  fields: string[];
  /** Each role its mapping gives a value. */
  roles: string[];
}

/** `view` as the editor changes it: without what the API works out. */
export function editable(view: ViewLayout): LayoutInput {
  return {
    columns: view.columns.map((column) => ({
      width: column.width,
      panels: column.panels.map(({ kind, height, stream, fields }) => ({
        kind,
        height,
        stream: stream ?? undefined,
        fields: [...fields],
      })),
    })),
  };
}

/**
 * `layout` as the view draws it, before the server says what each panel
 * lacks (`/api/layout/check`): by the same rules as every other layout.
 */
export function laidOut(layout: LayoutInput): ViewLayout {
  return {
    source: "own",
    columns: layout.columns.map((column) => ({
      width: column.width,
      panels: column.panels.map((panel): ViewPanel => ({
        kind: panel.kind,
        height: panel.height,
        stream: panel.stream ?? null,
        fields: panel.fields ?? [],
        missing: [],
      })),
    })),
  };
}

/** How many panels `layout` has. */
export function panelCount(layout: LayoutInput): number {
  return layout.columns.reduce((total, column) => total + column.panels.length, 0);
}

// Through JSON rather than structuredClone: a layout from the page's state
// holds its proxies, which can't be cloned.
const copy = (layout: LayoutInput): LayoutInput => JSON.parse(JSON.stringify(layout)) as LayoutInput;
const clamp = (value: number) => Math.min(MAX_SHARE, Math.max(1, value));

/** `layout` with `panel` added at the end of column `column`, a new column past the last; unchanged when full. */
export function added(layout: LayoutInput, column: number, panel: PanelInput): LayoutInput {
  if (panelCount(layout) >= MAX_PANELS) {
    return layout;
  }
  const next = copy(layout);
  if (column >= next.columns.length) {
    next.columns.push({ width: 1, panels: [] });
  }
  const into = next.columns[Math.min(column, next.columns.length - 1)]!;
  into.panels.push({ ...panel, fields: (panel.fields ?? []).slice(0, MAX_PLOT_FIELDS) });
  return next;
}

/** `layout` without the panel at `place` of `column`, and without a column left empty. */
export function removed(layout: LayoutInput, column: number, place: number): LayoutInput {
  const next = copy(layout);
  next.columns[column]?.panels.splice(place, 1);
  next.columns = next.columns.filter((each) => each.panels.length > 0);
  return next;
}

/** Where a panel moves. */
export type Move = "up" | "down" | "left" | "right";

/**
 * `layout` with the panel at `place` of `column` moved: up or down its
 * column, or to the end of the column beside it (a new one past the last).
 */
export function moved(layout: LayoutInput, column: number, place: number, move: Move): LayoutInput {
  const next = copy(layout);
  const from = next.columns[column];
  const panel = from?.panels[place];
  if (!from || !panel) {
    return layout;
  }
  if (move === "up" || move === "down") {
    const to = place + (move === "up" ? -1 : 1);
    if (to < 0 || to >= from.panels.length) {
      return layout;
    }
    from.panels[place] = from.panels[to]!;
    from.panels[to] = panel;
    return next;
  }
  const to = column + (move === "left" ? -1 : 1);
  if (to < 0) {
    return layout;
  }
  from.panels.splice(place, 1);
  if (to >= next.columns.length) {
    next.columns.push({ width: 1, panels: [] });
  }
  next.columns[to]!.panels.push(panel);
  next.columns = next.columns.filter((each) => each.panels.length > 0);
  return next;
}

/** `layout` with the panel at `place` of `column` made `step` taller (or shorter), from 1 to 12. */
export function heightened(layout: LayoutInput, column: number, place: number, step: number): LayoutInput {
  const next = copy(layout);
  const panel = next.columns[column]?.panels[place];
  if (panel) {
    panel.height = clamp(panel.height + step);
  }
  return next;
}

/** `layout` with column `column` made `step` wider (or narrower), from 1 to 12. */
export function widened(layout: LayoutInput, column: number, step: number): LayoutInput {
  const next = copy(layout);
  const each = next.columns[column];
  if (each) {
    each.width = clamp(each.width + step);
  }
  return next;
}

/** Where the operator's own layouts are kept in a browser. */
export interface LayoutStore {
  load(packageId: string): LayoutInput | null;
  save(packageId: string, layout: LayoutInput): void;
  clear(packageId: string): void;
}

const key = (packageId: string) => `kapto.layout.${packageId}`;

/** This browser's local storage; reading or writing it may fail (private windows), which leaves the layout unkept. */
export function browserLayouts(storage: Pick<Storage, "getItem" | "setItem" | "removeItem"> = localStorage): LayoutStore {
  return {
    load(packageId) {
      try {
        const text = storage.getItem(key(packageId));
        const read = text ? (JSON.parse(text) as LayoutInput) : null;
        return read && Array.isArray(read.columns) ? read : null;
      } catch {
        return null;
      }
    },
    save(packageId, layout) {
      try {
        storage.setItem(key(packageId), JSON.stringify(layout));
      } catch {
        // Not kept: the page still shows it until it is reloaded.
      }
    },
    clear(packageId) {
      try {
        storage.removeItem(key(packageId));
      } catch {
        // Nothing kept to forget.
      }
    },
  };
}
