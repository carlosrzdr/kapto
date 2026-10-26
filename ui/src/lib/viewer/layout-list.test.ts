import { describe, expect, it, vi } from "vitest";

import { ApiFailure } from "../api";
import type { LayoutInput, NamedLayout, NamedLayoutInput, NamedLayouts, ViewLayout } from "../api-types";
import {
  browserMemory,
  fileStem,
  type LayoutApi,
  MADE,
  offersOf,
  opensOn,
  PACKAGE,
  PackageLayouts,
  type ShownMemory,
  unique,
} from "./layout-list.svelte";

const view = (source: string, kind = "robot"): ViewLayout => ({
  source,
  columns: [{ width: 1, panels: [{ kind, height: 1, fields: [], missing: [] }] }],
});
const named = (id: string, name: string, kind = "health"): NamedLayout => ({
  id,
  name,
  layout: view(id === "default" ? "default" : "named", kind),
});
const listOf = (layouts: NamedLayout[], own = true): NamedLayouts => ({
  layouts,
  problems: [],
  package: own ? view("package", "camera") : null,
  made: view("made"),
});
const input = (kind: string): LayoutInput => ({ columns: [{ width: 1, panels: [{ kind, height: 1, fields: [] }] }] });

/** A server holding `layouts`, saving as the real one does, and a browser remembering `remembered`. */
function served(layouts: NamedLayout[], remembered: string | null = null, own = true) {
  const held = [...layouts];
  const asView = (layout: LayoutInput, id: string): ViewLayout => ({
    source: id === "default" ? "default" : "named",
    columns: layout.columns.map((column) => ({
      width: column.width,
      panels: column.panels.map((panel) => ({ ...panel, stream: panel.stream ?? null, fields: panel.fields ?? [], missing: [] })),
    })),
  });
  const taken = (name: string) =>
    new ApiFailure({ code: "layout.name_taken", message: `Another layout has that name: \`${name}\` is taken`, remedy: "Choose another name." });
  const api: LayoutApi = {
    list: vi.fn(async () => listOf([...held].sort((a, b) => a.name.localeCompare(b.name)), own)),
    create: vi.fn(async ({ name, columns }: NamedLayoutInput) => {
      if (held.some((each) => fileStem(each.name) === fileStem(name))) {
        throw taken(name);
      }
      const saved = { id: fileStem(name), name, layout: asView({ columns }, fileStem(name)) };
      held.push(saved);
      return saved;
    }),
    write: vi.fn(async (id: string, layout: LayoutInput) => {
      const was = held.findIndex((each) => each.id === id);
      const saved = { id, name: held[was]?.name ?? "Default", layout: asView(layout, id) };
      held.splice(was < 0 ? held.length : was, 1, saved);
      return saved;
    }),
    rename: vi.fn(async (id: string, name: string) => {
      const was = held.findIndex((each) => each.id === id);
      const saved = { ...held[was]!, id: fileStem(name), name };
      held.splice(was, 1, saved);
      return saved;
    }),
    remove: vi.fn(async (id: string) => held.splice(held.findIndex((each) => each.id === id), 1)),
    exported: vi.fn(async ({ name }: NamedLayoutInput) => ({ file: `${fileStem(name)}.toml`, text: `name = "${name}"\n` })),
    imported: vi.fn(async ({ text }: { file: string; text: string }) => {
      if (!text.startsWith("name")) {
        throw new ApiFailure({ code: "config.invalid_syntax", message: "`night.toml` line 1: The file isn't valid TOML", remedy: "Fix the line." });
      }
      return { name: "Wrist view", columns: input("camera").columns };
    }),
  };
  const kept = new Map<string, string>(remembered ? [["arm7", remembered]] : []);
  const memory: ShownMemory = { get: (id) => kept.get(id) ?? null, set: (id, shown) => void kept.set(id, shown) };
  return { api, kept, held, layouts: new PackageLayouts("arm7", api, memory) };
}

describe("the list of a package's layouts", () => {
  it("offers the named ones, then the package's own if it has one, then the one made from its roles", () => {
    const offers = offersOf(listOf([named("default", "Default"), named("night", "Night")]));
    expect(offers.map((each) => [each.id, each.name])).toEqual([
      ["default", "Default"],
      ["night", "Night"],
      [PACKAGE, "Package's"],
      [MADE, "Made from roles"],
    ]);
    expect(offersOf(listOf([], false)).map((each) => each.name)).toEqual(["Made from roles"]);
  });

  it("opens on the one this browser showed, else Default, else the package's, else the one made from its roles", () => {
    const all = offersOf(listOf([named("default", "Default"), named("night", "Night")]));
    expect(opensOn(all, "night")).toBe("night");
    expect(opensOn(all, "deleted-since")).toBe("default");
    expect(opensOn(offersOf(listOf([named("night", "Night")])), null)).toBe(PACKAGE);
    expect(opensOn(offersOf(listOf([named("night", "Night")], false)), null)).toBe(MADE);
  });

  it("names a copy so its file is not one taken, within the longest name", () => {
    expect(fileStem("Package's (edited)")).toBe("package-s-edited");
    expect(unique("Package's", " (edited)", ["Night"])).toBe("Package's (edited)");
    expect(unique("Package's", " (edited)", ["package's  EDITED"])).toBe("Package's (edited) 2");
    expect([...unique("x".repeat(64), " (copy)", [])]).toHaveLength(64);
  });

  it("is remembered per package by the browser, and does without when it keeps nothing", () => {
    const kept = new Map<string, string>();
    const memory = browserMemory({ getItem: (key) => kept.get(key) ?? null, setItem: (key, value) => void kept.set(key, value) });
    memory.set("arm7", "night");
    expect([memory.get("arm7"), memory.get("rover")]).toEqual(["night", null]);
    const refusing = browserMemory({
      getItem: () => {
        throw new Error("denied");
      },
      setItem: () => {
        throw new Error("denied");
      },
    });
    expect(() => refusing.set("arm7", "night")).not.toThrow();
    expect(refusing.get("arm7")).toBeNull();
  });
});

describe("arranging the one shown", () => {
  it("saves a named one over itself as it is arranged", async () => {
    const { api, layouts } = served([named("night", "Night")], "night");
    await layouts.load();
    layouts.arrange(input("events"));
    expect(layouts.view?.columns[0]?.panels[0]?.kind).toBe("events");
    await layouts.settled;
    expect(api.write).toHaveBeenCalledWith("night", input("events"));
    expect([layouts.shownId, layouts.view?.source]).toEqual(["night", "named"]);
  });

  it("saves the package's first as a layout of its own, made unique, which is then shown and saved over", async () => {
    const { api, kept, layouts } = served([named("package-s-edited", "Package's (edited)")]);
    await layouts.load();
    expect(layouts.shownId).toBe(PACKAGE);
    layouts.arrange(input("events"));
    layouts.arrange(input("plot"));
    await layouts.settled;
    // The first change was overtaken before it was saved: one layout made, from the last.
    expect(api.create).toHaveBeenCalledTimes(1);
    expect(api.create).toHaveBeenCalledWith({ name: "Package's (edited) 2", columns: input("plot").columns });
    expect([layouts.shownId, kept.get("arm7")]).toEqual(["package-s-edited-2", "package-s-edited-2"]);
    layouts.arrange(input("health"));
    await layouts.settled;
    expect(api.write).toHaveBeenCalledWith("package-s-edited-2", input("health"));
    expect(layouts.offers.map((each) => each.name)).toContain("Package's (edited) 2");
  });

  it("doesn't switch to what was saved once another is picked", async () => {
    const { layouts } = served([named("night", "Night")]);
    await layouts.load();
    layouts.pick(MADE);
    layouts.arrange(input("events"));
    layouts.pick("night");
    await layouts.settled;
    expect([layouts.shownId, layouts.view?.columns[0]?.panels[0]?.kind]).toEqual(["night", "health"]);
  });
});

describe("the list's actions", () => {
  it("renames, duplicates, deletes and saves the one shown as Default", async () => {
    const { api, layouts } = served([named("night", "Night")], "night");
    await layouts.load();
    await layouts.rename("Night shift");
    expect([layouts.shownId, layouts.shown?.name, layouts.said]).toEqual(["night-shift", "Night shift", "Renamed Night shift."]);
    await layouts.duplicate();
    expect([layouts.shownId, layouts.shown?.name]).toEqual(["night-shift-copy", "Night shift (copy)"]);
    await layouts.saveAsDefault();
    expect(api.write).toHaveBeenCalledWith("default", input("health"));
    expect(layouts.offers[0]?.name).toBe("Default");
    await layouts.remove();
    expect(api.remove).toHaveBeenCalledWith("night-shift-copy");
    expect([layouts.shownId, layouts.said]).toEqual(["default", "Deleted Night shift (copy)."]);
  });
});
