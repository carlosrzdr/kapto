// A layout as the operator arranges it in the page: each change a new
// layout, saved by the server, which says what each of its panels lacks.

import type { LayoutInput, PanelInput, PanelView, ViewLayout, ViewPanel } from "../api-types";

/** The most panels a page can draw, as `layout.toml` allows. */
export const MAX_PANELS = 24;
/** The most fields one plot draws. */
export const MAX_PLOT_FIELDS = 16;
/** The largest relative width or height. */
export const MAX_SHARE = 12;
/** The longest title a panel may have, in characters, as `layout.toml` allows. */
export const MAX_PANEL_TITLE = 64;
/** The most robot panels a layout may have: each is a draw of the whole scene a frame. */
export const MAX_ROBOT_VIEWS = 4;

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
      panels: column.panels.map(({ kind, height, stream, fields, title, view }) => ({
        kind,
        height,
        stream: stream ?? undefined,
        fields: [...fields],
        ...(title ? { title } : {}),
        ...(view ? { view: copyView(view) } : {}),
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
        title: panel.title ?? null,
        view: panel.view ?? null,
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

/** How many robot panels `layout` has. */
export function robotCount(layout: LayoutInput): number {
  return layout.columns.reduce((total, column) => total + column.panels.filter((panel) => panel.kind === "robot").length, 0);
}

const copyView = (view: PanelView): PanelView => (typeof view === "string" ? view : { from: [...view.from], at: [...view.at] });

/**
 * `layout` with `panel` added at the end of column `column`, a new column
 * past the last; unchanged when full, or when it is a fifth robot panel.
 */
export function added(layout: LayoutInput, column: number, panel: PanelInput): LayoutInput {
  if (panelCount(layout) >= MAX_PANELS || (panel.kind === "robot" && robotCount(layout) >= MAX_ROBOT_VIEWS)) {
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

/**
 * `title` as a panel's, by the rules `layout.toml` holds it to: on one
 * line, without the spaces around it, at most `MAX_PANEL_TITLE`
 * characters; `null` when nothing is left, which shows the title made from
 * what the panel shows.
 */
export function panelTitle(title: string): string | null {
  // Characters, not UTF-16 units, as the server counts them.
  const kept = [...title.replace(/\p{Cc}+/gu, " ").trim()].slice(0, MAX_PANEL_TITLE).join("").trim();
  return kept === "" ? null : kept;
}

/** `layout` with the panel at `place` of `column` titled `title`, or without a title of its own when it is blank. */
export function retitled(layout: LayoutInput, column: number, place: number, title: string): LayoutInput {
  const next = copy(layout);
  const panel = next.columns[column]?.panels[place];
  if (!panel) {
    return layout;
  }
  const kept = panelTitle(title);
  if (kept === null) {
    delete panel.title;
  } else {
    panel.title = kept;
  }
  return next;
}

/** `layout` with the robot panel at `place` of `column` seen from `view`, or framing the robot when `null`. */
export function withView(layout: LayoutInput, column: number, place: number, view: PanelView | null): LayoutInput {
  const next = copy(layout);
  const panel = next.columns[column]?.panels[place];
  if (panel?.kind !== "robot") {
    return layout;
  }
  if (view === null) {
    delete panel.view;
  } else {
    panel.view = copyView(view);
  }
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

/** Where a dragged panel is dropped: into a column before its panel `at`, or as a new column `between` two. */
export type Place = { column: number; at: number } | { between: number };

/**
 * `layout` with the panel at `at` of `column` dropped at `to`, as when it
 * is dragged: a column it leaves empty goes, and a new one is as wide as
 * the narrowest beside it.
 */
export function placed(layout: LayoutInput, column: number, at: number, to: Place): LayoutInput {
  const next = copy(layout);
  const panel = next.columns[column]?.panels[at];
  if (!panel) {
    return layout;
  }
  if ("between" in to) {
    const between = Math.max(0, Math.min(to.between, next.columns.length));
    // Dropped beside the column it is alone in: nothing moves.
    if (next.columns[column]!.panels.length === 1 && (between === column || between === column + 1)) {
      return layout;
    }
    const beside = [next.columns[between - 1], next.columns[between]].filter((each) => each !== undefined);
    const width = Math.min(...beside.map((each) => each.width));
    next.columns[column]!.panels.splice(at, 1);
    next.columns.splice(between, 0, { width, panels: [panel] });
  } else {
    const into = next.columns[to.column];
    if (!into) {
      return layout;
    }
    // Before the panel it was dropped on, as it stood with this one still there.
    let before = Math.max(0, Math.min(to.at, into.panels.length));
    if (to.column === column && before > at) {
      before -= 1;
    }
    next.columns[column]!.panels.splice(at, 1);
    into.panels.splice(before, 0, panel);
  }
  next.columns = next.columns.filter((each) => each.panels.length > 0);
  return next;
}

/**
 * Sizes on the page, in pixels, as shares of 1 to 12 in the same
 * proportions, as near as whole shares come: the largest is 12.
 */
export function apportioned(pixels: number[]): number[] {
  const largest = Math.max(...pixels, 1);
  return pixels.map((each) => clamp(Math.round((each / largest) * MAX_SHARE)));
}

/**
 * A column's panels' sizes on the page, in pixels, as whole shares of 1 to
 * 12, where a share is as many pixels as `shares` gave each panel when
 * they were measured. A share is a size of its own (a panel's least, two
 * shares twice it, and more where the column has room), so a size dragged
 * in a column that scrolls becomes as many shares as it holds, and one not
 * yet dragged keeps the share it had.
 */
export function reshared(pixels: number[], shares: number[]): number[] {
  const total = shares.reduce((sum, each) => sum + each, 0);
  const each = pixels.reduce((sum, size) => sum + size, 0) / Math.max(total, 1) || 1;
  return pixels.map((size) => clamp(Math.round(size / each)));
}

/**
 * `shares` with the edge after `edge` moved `step` shares: the one before
 * grows by as much as the one after shrinks, neither past 1 to 12: where
 * one can't change the other still does, so an edge between two at their
 * least still moves. `null` when neither can change.
 */
export function nudged(shares: number[], edge: number, step: number): number[] | null {
  const before = shares[edge];
  const after = shares[edge + 1];
  if (before === undefined || after === undefined) {
    return null;
  }
  const next = [...shares];
  next[edge] = clamp(before + step);
  next[edge + 1] = clamp(after - step);
  return next[edge] === before && next[edge + 1] === after ? null : next;
}

/** `layout` with its columns `widths` wide; any not given stay as they are. */
export function withWidths(layout: LayoutInput, widths: number[]): LayoutInput {
  const next = copy(layout);
  next.columns.forEach((each, place) => {
    each.width = clamp(widths[place] ?? each.width);
  });
  return next;
}

/** `layout` with the panels of `column` `heights` tall; any not given stay as they are. */
export function withHeights(layout: LayoutInput, column: number, heights: number[]): LayoutInput {
  const next = copy(layout);
  next.columns[column]?.panels.forEach((panel, place) => {
    panel.height = clamp(heights[place] ?? panel.height);
  });
  return next;
}
