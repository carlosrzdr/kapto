// The layouts a package's view may be arranged by, chosen from one list:
// its named layouts in the config folder (Default first), its own and the
// one made from what it declares. The layouts are the server's, saved as
// they are arranged; the browser remembers only which one it shows.

import {
  ApiFailure,
  createLayout,
  deleteLayout,
  exportLayout,
  getLayouts,
  importLayout,
  renameLayout,
  writeLayout,
} from "../api";
import type { LayoutFile, LayoutInput, NamedLayout, NamedLayoutInput, NamedLayouts, ViewLayout } from "../api-types";
import { sentence } from "../format";
import { editable, laidOut } from "./layouts";
import { type OfferMemory, offerMemory, packageSeen } from "./layout-offer";

/** *Default*'s id among the named layouts. */
export const DEFAULT = "default";
/** The package's own layout in the list; a named layout's id, a file's stem, never starts with `:`. */
export const PACKAGE = ":package";
/** The layout made from what the package declares, in the list. */
export const MADE = ":made";
/** The longest name a layout may have, in characters, as the server allows. */
export const MAX_NAME = 64;

/** One layout the list offers. */
export interface Offer {
  id: string;
  name: string;
  layout: ViewLayout;
}

/** Whether `id` is one of the named layouts, which are renamed, deleted and saved over; the other two follow the package. */
export const isNamed = (id: string): boolean => !id.startsWith(":");

/** What the list offers, in its order: the named ones as the server sorts them, then *Package's* if there is one, then *Made from roles*. */
export function offersOf(list: NamedLayouts): Offer[] {
  return [
    ...list.layouts.map(({ id, name, layout }) => ({ id, name, layout })),
    ...(list.package ? [{ id: PACKAGE, name: "Package's", layout: list.package }] : []),
    { id: MADE, name: "Made from roles", layout: list.made },
  ];
}

/** The one the page opens on: the one this browser showed last, else *Default*, else *Package's*, else *Made from roles*. */
export function opensOn(offers: Offer[], remembered: string | null): string {
  for (const id of [remembered, DEFAULT, PACKAGE]) {
    if (id !== null && offers.some((offer) => offer.id === id)) {
      return id;
    }
  }
  return MADE;
}

/**
 * The file a layout named `name` is saved in, without `.toml`, by the
 * server's rule: its letters and digits in lower case, every run of
 * anything else one `-`. Two names with one file are one name taken twice.
 */
export function fileStem(name: string): string {
  let stem = "";
  for (const each of name) {
    if (/[\p{L}\p{N}]/u.test(each)) {
      stem += each.toLowerCase();
    } else if (stem !== "" && !stem.endsWith("-")) {
      stem += "-";
    }
  }
  return stem.replace(/-+$/, "");
}

/** `base` followed by `suffix`, made unique among `names` by a number after it, within `MAX_NAME` characters. */
export function unique(base: string, suffix: string, names: string[]): string {
  const taken = new Set(names.map(fileStem));
  for (let count = 1; ; count += 1) {
    const end = count === 1 ? suffix : `${suffix} ${count}`;
    const name = [...base].slice(0, MAX_NAME - [...end].length).join("").trimEnd() + end;
    if (!taken.has(fileStem(name))) {
      return name;
    }
  }
}

/** `list` with `saved` in it, in place of the one it was or among the others, in the server's order. */
export function withSaved(list: NamedLayouts, saved: NamedLayout, was: string = saved.id): NamedLayouts {
  const layouts = [...list.layouts.filter((each) => each.id !== was && each.id !== saved.id), saved];
  layouts.sort((a, b) => Number(a.id !== DEFAULT) - Number(b.id !== DEFAULT) || a.name.toLowerCase().localeCompare(b.name.toLowerCase()));
  return { ...list, layouts };
}

/** Where a browser remembers which layout it shows, per package. */
export interface ShownMemory {
  get(packageId: string): string | null;
  set(packageId: string, id: string): void;
}

const shownKey = (packageId: string) => `kapto.layout-shown.${packageId}`;

/** This browser's local storage; reading or writing it may fail (private windows), which leaves the choice unkept. */
export function browserMemory(storage?: Pick<Storage, "getItem" | "setItem">): ShownMemory {
  return {
    get(packageId) {
      try {
        return (storage ?? localStorage).getItem(shownKey(packageId));
      } catch {
        return null;
      }
    },
    set(packageId, id) {
      try {
        (storage ?? localStorage).setItem(shownKey(packageId), id);
      } catch {
        // Not kept: the page opens on Default after a reload.
      }
    },
  };
}

/** The calls the list makes, which tests replace. */
export interface LayoutApi {
  list(): Promise<NamedLayouts>;
  create(layout: NamedLayoutInput): Promise<NamedLayout>;
  write(id: string, layout: LayoutInput): Promise<NamedLayout>;
  rename(id: string, name: string): Promise<NamedLayout>;
  remove(id: string): Promise<unknown>;
  exported(layout: NamedLayoutInput): Promise<LayoutFile>;
  imported(file: LayoutFile): Promise<NamedLayoutInput>;
}

export const serverLayouts: LayoutApi = {
  list: getLayouts,
  create: createLayout,
  write: writeLayout,
  rename: renameLayout,
  remove: deleteLayout,
  exported: exportLayout,
  imported: importLayout,
};

/** The cause codes of a name the server won't save a layout under: asked for another. */
const NAME_REFUSED = new Set(["layout.name_taken", "layout.name_empty", "layout.name_too_long"]);

/** A package's layouts as the page offers them, the one shown, and what the list last did. */
export class PackageLayouts {
  /** The list as the server last gave it; none until it has. */
  list = $state.raw<NamedLayouts | null>(null);
  /** The one shown, by id; none until the list is read. */
  shownId = $state<string | null>(null);
  /** The one shown as arranged on this page, ahead of what the server answered. */
  arranged = $state.raw<LayoutInput | null>(null);
  /** That as the server saved it, each panel with what the package lacks of it. */
  private checked = $state.raw<ViewLayout | null>(null);
  /** What the last change did, or why it failed, with its remedy. */
  said = $state("");
  failed = $state(false);
  /** A layout being imported whose name the server refused: saved once given another. */
  clash = $state.raw<NamedLayoutInput | null>(null);
  /**
   * The package's own layout as this browser last saw it, while the
   * package's differs from it: the new one offered, never applied.
   */
  stale = $state.raw<LayoutInput | null>(null);

  readonly offers = $derived(this.list ? offersOf(this.list) : []);
  readonly shown = $derived(this.offers.find((offer) => offer.id === this.shownId) ?? null);
  /** How the view is arranged: as arranged here, else the one shown; none before the list is read. */
  readonly view = $derived(this.arranged ? (this.checked ?? laidOut(this.arranged)) : (this.shown?.layout ?? null));

  /** Changes since the one shown was picked; a save for an earlier one changes nothing on the page. */
  private picks = 0;
  /** Saves and the list's actions, one at a time, so each finds the layout as the one before left it. */
  private queue: Promise<void> = Promise.resolve();

  constructor(
    readonly packageId: string,
    private readonly api: LayoutApi = serverLayouts,
    private readonly memory: ShownMemory = browserMemory(),
    private readonly seenMemory: OfferMemory = offerMemory(),
  ) {}

  /** Settles once every change so far is saved, or has failed. */
  get settled(): Promise<void> {
    return this.queue;
  }

  /** Reads the list, opening on the one to open on while none is shown or the one shown has gone. */
  async load(): Promise<void> {
    try {
      this.list = await this.api.list();
      if (!this.offers.some((offer) => offer.id === this.shownId)) {
        this.shownId = opensOn(this.offers, this.memory.get(this.packageId));
      }
      const seen = packageSeen(this.seenMemory.seen(this.packageId), this.list.package);
      if (seen.kind === "first") {
        // Nothing to compare it with: kept as seen without asking.
        this.seeNow();
      }
      this.stale = seen.kind === "changed" ? seen.old : null;
    } catch (error) {
      this.say(error);
    }
  }

  /** Reads the list again once what is being saved is, for what another browser saved since. */
  refresh(): Promise<void> {
    return this.queued(() => this.load());
  }

  /** Shows the layout `id`, remembered in this browser. */
  pick(id: string) {
    this.picks += 1;
    this.shownId = id;
    this.arranged = null;
    this.checked = null;
    this.said = "";
    this.clash = null;
    this.memory.set(this.packageId, id);
  }

  /**
   * Shows `next` at once and saves it: over the named layout shown, or,
   * for *Package's* and *Made from roles*, which follow the package, first
   * as a named layout of its own, which is then the one shown.
   */
  arrange(next: LayoutInput) {
    this.arranged = next;
    this.checked = null;
    this.said = "";
    const pick = this.picks;
    this.queued(() => this.save(next, pick));
  }

  private async save(next: LayoutInput, pick: number) {
    const id = this.shownId;
    // A newer change saves itself; one picked away from isn't saved.
    if (pick !== this.picks || next !== this.arranged || id === null) {
      return;
    }
    const saved = isNamed(id)
      ? await this.api.write(id, next)
      : await this.api.create({ name: unique(this.shown?.name ?? "Layout", " (edited)", this.names()), columns: next.columns });
    this.keep(saved);
    if (pick !== this.picks) {
      return;
    }
    if (saved.id !== id) {
      this.shownId = saved.id;
      this.memory.set(this.packageId, saved.id);
    }
    if (this.arranged === next) {
      this.checked = saved.layout;
    }
  }

  /** Renames the one shown, if it is named. */
  rename(name: string): Promise<void> {
    return this.queued(async () => {
      const id = this.shownId;
      if (id === null || !isNamed(id)) {
        return;
      }
      const saved = await this.api.rename(id, name.trim());
      this.keep(saved, id);
      this.showSaved(saved);
      this.tell(`Renamed ${saved.name}.`);
    });
  }

  /** Saves the one shown, as it is shown, under a name of its own, which is then shown. */
  duplicate(): Promise<void> {
    return this.queued(async () => {
      const shown = this.shown;
      const layout = this.view;
      if (!shown || !layout) {
        return;
      }
      const saved = await this.api.create({ name: unique(shown.name, " (copy)", this.names()), columns: editable(layout).columns });
      this.keep(saved);
      this.pick(saved.id);
      this.tell(`Saved as ${saved.name}.`);
    });
  }

  /** Deletes the one shown, if it is named, and shows the one a browser would open on. */
  remove(): Promise<void> {
    return this.queued(async () => {
      const shown = this.shown;
      if (!shown || !isNamed(shown.id)) {
        return;
      }
      await this.api.remove(shown.id);
      this.list = await this.api.list();
      this.pick(opensOn(this.offers, null));
      this.tell(`Deleted ${shown.name}.`);
    });
  }

  /** Saves the one shown, as it is shown, as *Default*: what a browser that hasn't picked one opens on. */
  saveAsDefault(): Promise<void> {
    return this.queued(async () => {
      const layout = this.view;
      if (!layout || this.shownId === DEFAULT) {
        return;
      }
      this.keep(await this.api.write(DEFAULT, editable(layout)));
      this.tell("Saved as Default: a browser that hasn't picked a layout opens on it.");
    });
  }

  /** The one shown as a file, named from it; none if it can't be. */
  async exportShown(): Promise<LayoutFile | null> {
    await this.queue;
    const shown = this.shown;
    const layout = this.view;
    if (!shown || !layout) {
      return null;
    }
    try {
      return await this.api.exported({ name: shown.name, columns: editable(layout).columns });
    } catch (error) {
      this.say(error);
      return null;
    }
  }

  /** Adds the layout in the file `file`, its text `text`, as a new named layout, asking for another name if its own is refused. */
  importFile(file: string, text: string): Promise<void> {
    return this.queued(async () => this.add(await this.api.imported({ file, text })));
  }

  /** Saves the layout being imported under `name`, its own having been refused. */
  importAs(name: string): Promise<void> {
    const clash = this.clash;
    return clash ? this.queued(() => this.add({ ...clash, name: name.trim() })) : Promise.resolve();
  }

  /** Gives up the import whose name was refused. */
  cancelImport() {
    this.clash = null;
    this.said = "";
  }

  /** Shows the package's new layout, which is then the one seen. */
  useNew() {
    this.seeNow();
    this.pick(PACKAGE);
  }

  /** Keeps the one shown; the package's new layout is then the one seen, and not offered again. */
  keepThis() {
    this.seeNow();
  }

  /** Saves the package's old layout under `name`, then shows the new one; why it couldn't be, if it couldn't. */
  saveOld(name: string): Promise<string | null> {
    const old = this.stale;
    return old
      ? this.saveNamed(name, old, () => {
          this.seeNow();
          this.pick(PACKAGE);
        })
      : Promise.resolve(null);
  }

  /** Saves `layout` under `name`, which is then shown; why it couldn't be, if it couldn't. */
  saveAs(name: string, layout: LayoutInput): Promise<string | null> {
    return this.saveNamed(name, layout, (saved) => this.pick(saved.id));
  }

  /** A name for a layout saved from `base`, not yet taken. */
  nameFor(base: string, suffix: string): string {
    return unique(base, suffix, this.names());
  }

  private seeNow() {
    const now = this.list?.package;
    if (now) {
      this.seenMemory.see(this.packageId, editable(now));
    }
    this.stale = null;
  }

  private saveNamed(name: string, layout: LayoutInput, then: (saved: NamedLayout) => void): Promise<string | null> {
    let failed: string | null = null;
    this.queue = this.queue.then(async () => {
      try {
        const saved = await this.api.create({ name: name.trim(), columns: layout.columns });
        this.keep(saved);
        then(saved);
        this.tell(`Saved as ${saved.name}.`);
      } catch (error) {
        failed = reason(error);
      }
    });
    return this.queue.then(() => failed);
  }

  /** Forgets what the last change said. */
  dismiss() {
    this.said = "";
    this.failed = false;
  }

  private async add(layout: NamedLayoutInput) {
    try {
      const saved = await this.api.create(layout);
      this.clash = null;
      this.keep(saved);
      this.pick(saved.id);
      this.tell(`Imported ${saved.name}.`);
    } catch (error) {
      if (error instanceof ApiFailure && NAME_REFUSED.has(error.code)) {
        this.clash = layout;
      }
      throw error;
    }
  }

  private names(): string[] {
    return this.list?.layouts.map((each) => each.name) ?? [];
  }

  private keep(saved: NamedLayout, was?: string) {
    if (this.list) {
      this.list = withSaved(this.list, saved, was);
    }
  }

  /** Shows `saved` in place of the one it was, as arranged here if it is. */
  private showSaved(saved: NamedLayout) {
    this.shownId = saved.id;
    this.memory.set(this.packageId, saved.id);
  }

  private queued(work: () => Promise<void>): Promise<void> {
    this.queue = this.queue.then(work).catch((error: unknown) => this.say(error));
    return this.queue;
  }

  private tell(done: string) {
    this.said = done;
    this.failed = false;
  }

  private say(error: unknown) {
    this.said = reason(error);
    this.failed = true;
  }
}

/** Why `error` happened and what to do, in a sentence or two. */
const reason = (error: unknown): string =>
  error instanceof ApiFailure ? `${sentence(error.message)} ${error.remedy}` : String(error);
