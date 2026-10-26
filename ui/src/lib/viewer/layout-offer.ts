// Another layout than the one shown, offered and never applied: the
// package's own when it has changed since this browser last saw it, and
// the one an episode was recorded with when it isn't the one shown.

import type { LayoutInput, ViewLayout } from "../api-types";

/** A layout's columns and panels, without where it comes from or what a package lacks of it. */
type Arranged = LayoutInput | ViewLayout;

/**
 * `layout` as one line of text, the same for two layouts that arrange the
 * view the same: the columns and panels the server parsed, so the file's
 * spacing and comments are already gone, with keys in one order, an
 * absent title, stream or fields as an empty one, and neither `source` nor
 * `missing`, which say where it comes from and what a package lacks of it.
 */
export function canonical(layout: Arranged): string {
  return JSON.stringify(
    layout.columns.map((column) => [
      column.width,
      column.panels.map((panel) => [panel.kind, panel.height, panel.stream ?? null, panel.fields ?? [], panel.title || null]),
    ]),
  );
}

/** Whether `a` and `b` arrange the view the same. */
export const sameLayout = (a: Arranged, b: Arranged): boolean => canonical(a) === canonical(b);

/** What to do with the package's own layout as the page reads it now. */
export type PackageSeen =
  /** Nothing seen before: keep it as seen, without asking. */
  | { kind: "first" }
  /** It changed: offer the new one, and the old one to keep under a name. */
  | { kind: "changed"; old: LayoutInput }
  /** As seen, or the package has none: nothing to say. */
  | { kind: "same" };

/** How the package's layout `now` compares with `seen`, the one this browser last saw. */
export function packageSeen(seen: LayoutInput | null, now: ViewLayout | null | undefined): PackageSeen {
  if (!now) {
    // A package without a layout of its own has nothing new to offer; what was seen stays.
    return { kind: "same" };
  }
  if (seen === null) {
    return { kind: "first" };
  }
  return sameLayout(seen, now) ? { kind: "same" } : { kind: "changed", old: seen };
}

/**
 * Whether a replay offers the layout its episode was recorded with: only
 * one the recording carries (its package's `layout.toml`, not one made
 * from what it declares), other than the one shown, and only until it has
 * been answered for that episode.
 */
export function offersEpisode(carried: ViewLayout | null | undefined, shown: ViewLayout, answered: boolean): carried is ViewLayout {
  return !answered && carried?.source === "package" && !sameLayout(carried, shown);
}

/** How many episodes' answers a browser keeps, the newest: an old episode replayed again past them is asked again. */
export const ANSWERS_KEPT = 500;

/** What a browser remembers of what it was offered. */
export interface OfferMemory {
  /** The package's own layout as this browser last saw it. */
  seen(packageId: string): LayoutInput | null;
  see(packageId: string, layout: LayoutInput): void;
  /** Whether the layout episode `id` was recorded with has been answered. */
  answered(id: string): boolean;
  answer(id: string): void;
}

const seenKey = (packageId: string) => `kapto.layout-seen.${packageId}`;
const ANSWERED = "kapto.layout-answered";

/** This browser's local storage; reading or writing it may fail (private windows), which only asks again. */
export function offerMemory(storage?: Pick<Storage, "getItem" | "setItem">): OfferMemory {
  const store = () => storage ?? localStorage;
  const read = <T>(key: string, fallback: T): T => {
    try {
      const text = store().getItem(key);
      return text === null ? fallback : (JSON.parse(text) as T);
    } catch {
      return fallback;
    }
  };
  const write = (key: string, value: unknown) => {
    try {
      store().setItem(key, JSON.stringify(value));
    } catch {
      // Not kept: asked again after a reload.
    }
  };
  const answers = () => {
    const ids = read<unknown>(ANSWERED, []);
    return Array.isArray(ids) ? ids.filter((id): id is string => typeof id === "string") : [];
  };
  return {
    seen(packageId) {
      const layout = read<LayoutInput | null>(seenKey(packageId), null);
      return layout && Array.isArray(layout.columns) ? layout : null;
    },
    see(packageId, layout) {
      write(seenKey(packageId), layout);
    },
    answered(id) {
      return answers().includes(id);
    },
    answer(id) {
      write(ANSWERED, [...answers().filter((each) => each !== id), id].slice(-ANSWERS_KEPT));
    },
  };
}
