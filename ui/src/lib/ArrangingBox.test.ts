import { fireEvent, render, screen } from "@testing-library/svelte";
import { createRawSnippet } from "svelte";
import { describe, expect, it } from "vitest";

import ArrangingBox, { ARRANGING_KEY, rememberedPlace } from "./ArrangingBox.svelte";

/** A browser's storage, as a map. */
function storage(kept = new Map<string, string>()) {
  return { kept, store: { getItem: (key: string) => kept.get(key) ?? null, setItem: (key: string, value: string) => void kept.set(key, value) } as Storage };
}

const children = createRawSnippet(() => ({ render: () => "<p>What the editor offers</p>" }));

function box(store: Storage, faded = false) {
  return render(ArrangingBox, { faded, storage: store, children });
}

describe("the arranging editor's box", () => {
  it("opens unfolded on the right the first time", () => {
    const { store } = storage();
    box(store);
    expect(screen.getByText("What the editor offers")).toBeTruthy();
    expect(screen.getByRole("button", { name: "Fold the editor" }).getAttribute("aria-expanded")).toBe("true");
    expect(screen.getByRole("button", { name: "Move to the left" })).toBeTruthy();
  });

  it("folds to its bar and unfolds again, remembering it", async () => {
    const { kept, store } = storage();
    const { container } = box(store);
    await fireEvent.click(screen.getByRole("button", { name: "Fold the editor" }));
    const unfold = screen.getByRole("button", { name: "Unfold the editor" });
    expect(unfold.getAttribute("aria-expanded")).toBe("false");
    expect(container.querySelector<HTMLElement>("#arranging-body")!.hidden).toBe(true);
    expect(JSON.parse(kept.get(ARRANGING_KEY)!)).toEqual({ folded: true, side: "right" });
    await fireEvent.click(unfold);
    expect(container.querySelector<HTMLElement>("#arranging-body")!.hidden).toBe(false);
    expect(JSON.parse(kept.get(ARRANGING_KEY)!)).toEqual({ folded: false, side: "right" });
  });

  it("moves to the left and back, opening where it was left", async () => {
    const { kept, store } = storage();
    const first = box(store);
    await fireEvent.click(screen.getByRole("button", { name: "Move to the left" }));
    expect(first.container.querySelector("#arranging")!.classList.contains("left")).toBe(true);
    first.unmount();
    const again = box(store);
    expect(again.container.querySelector("#arranging")!.classList.contains("left")).toBe(true);
    await fireEvent.click(screen.getByRole("button", { name: "Move to the right" }));
    expect(JSON.parse(kept.get(ARRANGING_KEY)!)).toEqual({ folded: false, side: "right" });
  });

  it("fades while something is dragged", () => {
    const { store } = storage();
    const { container } = box(store, true);
    expect(container.querySelector("#arranging")!.classList.contains("faded")).toBe(true);
  });

  it("reads storage it cannot parse, or cannot reach, as never left", () => {
    const { store } = storage(new Map([[ARRANGING_KEY, "{not json"]]));
    expect(rememberedPlace(store)).toEqual({ folded: false, side: "right" });
    const unreachable = {
      getItem: () => {
        throw new Error("blocked");
      },
    } as unknown as Storage;
    expect(rememberedPlace(unreachable)).toEqual({ folded: false, side: "right" });
    expect(rememberedPlace(storage(new Map([[ARRANGING_KEY, '{"folded":true,"side":"up"}']])).store)).toEqual({ folded: true, side: "right" });
  });

  it("keeps working when the storage cannot be written", async () => {
    const blocked = {
      getItem: () => null,
      setItem: () => {
        throw new Error("blocked");
      },
    } as unknown as Storage;
    box(blocked);
    await fireEvent.click(screen.getByRole("button", { name: "Fold the editor" }));
    expect(screen.getByRole("button", { name: "Unfold the editor" })).toBeTruthy();
  });
});
