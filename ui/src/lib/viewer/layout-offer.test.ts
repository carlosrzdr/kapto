import { describe, expect, it, vi } from "vitest";

import type { LayoutInput, NamedLayoutInput, NamedLayouts, ViewLayout } from "../api-types";
import { type LayoutApi, PACKAGE, PackageLayouts, type ShownMemory } from "./layout-list.svelte";
import { canonical, offerMemory, offersEpisode, packageSeen, sameLayout } from "./layout-offer";

const panel = (kind: string, more: object = {}) => ({ kind, height: 1, fields: [] as string[], missing: [] as string[], ...more });
const view = (source: string, ...kinds: string[]): ViewLayout => ({
  source,
  columns: [{ width: 1, panels: kinds.map((kind) => panel(kind)) }],
});
const input = (...kinds: string[]): LayoutInput => ({ columns: [{ width: 1, panels: kinds.map((kind) => ({ kind, height: 1, fields: [] })) }] });

/** A browser's storage, as a map. */
function storage() {
  const kept = new Map<string, string>();
  return { kept, store: { getItem: (key: string) => kept.get(key) ?? null, setItem: (key: string, value: string) => void kept.set(key, value) } };
}

describe("two layouts compared as parsed", () => {
  it("are the same whatever order their keys came in, as two files laid out differently parse", () => {
    // The server parses `layout.toml`, so spacing and comments never reach
    // the page; what is left of a file written another way is its keys' order.
    const one: ViewLayout = {
      source: "package",
      columns: [{ width: 2, panels: [{ kind: "plot", height: 1, fields: ["arm.q[0]"], missing: [], title: "Shoulder" }] }],
    };
    const other: ViewLayout = {
      columns: [{ panels: [{ title: "Shoulder", missing: [], fields: ["arm.q[0]"], height: 1, kind: "plot" }], width: 2 }],
      source: "package",
    };
    expect(sameLayout(one, other)).toBe(true);
  });

  it("leave out where each comes from and what a package lacks of it, and read an absent title, stream or fields as none", () => {
    const shown: ViewLayout = {
      source: "named",
      columns: [{ width: 1, panels: [panel("camera", { stream: "wrist", missing: ["no stream `wrist`"], title: null })] }],
    };
    const carried: LayoutInput = { columns: [{ width: 1, panels: [{ kind: "camera", height: 1, stream: "wrist" }] }] };
    expect(canonical(shown)).toBe(canonical(carried));
    expect(sameLayout(view("package", "robot"), view("made", "robot"))).toBe(true);
  });

  it("differ by a panel, its order, its share or what it shows", () => {
    const base = view("package", "robot", "events");
    expect(sameLayout(base, view("package", "events", "robot"))).toBe(false);
    expect(sameLayout(base, view("package", "robot"))).toBe(false);
    expect(sameLayout(base, { ...base, columns: [{ ...base.columns[0]!, width: 2 }] })).toBe(false);
    const titled = { source: "package", columns: [{ width: 1, panels: [panel("robot", { title: "Arm" }), panel("events")] }] };
    expect(sameLayout(base, titled)).toBe(false);
  });
});

describe("the package's own layout", () => {
  it("is kept without asking the first time, offered once it has changed, and nothing while it is as seen or gone", () => {
    expect(packageSeen(null, view("package", "robot"))).toEqual({ kind: "first" });
    expect(packageSeen(input("robot"), view("package", "robot"))).toEqual({ kind: "same" });
    expect(packageSeen(input("robot"), view("package", "events"))).toEqual({ kind: "changed", old: input("robot") });
    expect(packageSeen(input("robot"), null)).toEqual({ kind: "same" });
  });
});

describe("the layout an episode was recorded with", () => {
  it("is offered when the recording carries its own other than the one shown, until it has been answered", () => {
    const shown = view("named", "robot", "events");
    expect(offersEpisode(view("package", "robot"), shown, false)).toBe(true);
    expect(offersEpisode(view("package", "robot"), shown, true)).toBe(false);
    expect(offersEpisode(view("package", "events", "robot"), view("named", "events", "robot"), false)).toBe(false);
    // One made from what the recording declares is no layout it carries.
    expect(offersEpisode(view("made", "robot"), shown, false)).toBe(false);
    expect(offersEpisode(null, shown, false)).toBe(false);
  });

  it("is asked once per episode: answered for one, the others are still asked", () => {
    const { store } = storage();
    const memory = offerMemory(store);
    memory.answer("ep-1");
    expect([memory.answered("ep-1"), memory.answered("ep-2")]).toEqual([true, false]);
    // Another page in the same browser finds it answered too.
    expect(offerMemory(store).answered("ep-1")).toBe(true);
  });

  it("is asked again in a browser that keeps nothing, which never fails the page", () => {
    const refusing = offerMemory({
      getItem: () => {
        throw new Error("denied");
      },
      setItem: () => {
        throw new Error("denied");
      },
    });
    expect(() => refusing.answer("ep-1")).not.toThrow();
    expect(() => refusing.see("arm7", input("robot"))).not.toThrow();
    expect([refusing.answered("ep-1"), refusing.seen("arm7")]).toEqual([false, null]);
  });
});

/** A server whose package's own layout is `own` until changed, and a browser keeping what it saw in `kept`. */
function served(own: ViewLayout) {
  const list = { now: own };
  const created: NamedLayoutInput[] = [];
  const api = {
    list: vi.fn(async (): Promise<NamedLayouts> => ({ layouts: [], problems: [], package: list.now, made: view("made", "robot") })),
    create: vi.fn(async (layout: NamedLayoutInput) => {
      created.push(layout);
      return { id: "old", name: layout.name, layout: { source: "named", columns: view("named", "robot").columns } };
    }),
  } as unknown as LayoutApi;
  const shown = new Map<string, string>();
  const memory: ShownMemory = { get: (id) => shown.get(id) ?? null, set: (id, each) => void shown.set(id, each) };
  const { kept, store } = storage();
  const page = () => new PackageLayouts("arm7", api, memory, offerMemory(store));
  return { list, created, kept, page };
}

describe("a package's layout changed since the browser saw it", () => {
  it("is kept as seen without asking on the first visit, and offered on the next once changed", async () => {
    const { list, page } = served(view("package", "robot"));
    const first = page();
    await first.load();
    expect(first.stale).toBeNull();
    list.now = view("package", "robot", "events");
    const next = page();
    await next.load();
    expect(next.stale).toEqual(input("robot"));
    // Offered, never applied: the one shown stays.
    expect(next.shownId).toBe(PACKAGE);
  });

  it("shows the new one when asked, which is then the one seen", async () => {
    const { list, page } = served(view("package", "robot"));
    await page().load();
    list.now = view("package", "events");
    const layouts = page();
    await layouts.load();
    layouts.pick("night");
    layouts.useNew();
    expect([layouts.stale, layouts.shownId]).toEqual([null, PACKAGE]);
    const again = page();
    await again.load();
    expect(again.stale).toBeNull();
  });

  it("saves the old one under a name, then shows the new one", async () => {
    const { list, created, page } = served(view("package", "robot"));
    await page().load();
    list.now = view("package", "events");
    const layouts = page();
    await layouts.load();
    expect(await layouts.saveOld("Package's (old)")).toBeNull();
    expect(created).toEqual([{ name: "Package's (old)", columns: input("robot").columns }]);
    expect([layouts.stale, layouts.shownId, layouts.said]).toEqual([null, PACKAGE, "Saved as Package's (old)."]);
  });

  it("is kept as it is shown when kept, and not offered again", async () => {
    const { list, page } = served(view("package", "robot"));
    await page().load();
    list.now = view("package", "events");
    const layouts = page();
    await layouts.load();
    layouts.keepThis();
    expect(layouts.stale).toBeNull();
    const again = page();
    await again.load();
    expect(again.stale).toBeNull();
  });
});
