import { fireEvent, render, screen } from "@testing-library/svelte";
import { describe, expect, it, vi } from "vitest";

import { ApiFailure } from "./api";
import type { EventSent, SendEvent, VocabularyKind } from "./api-types";
import MarkBar from "./MarkBar.svelte";

const kinds: VocabularyKind[] = [
  { kind: "note", name: "Note", stretch: false, from: "default" },
  { kind: "intervention", name: "Intervention", stretch: true, from: "default" },
];

/** A send that answers as capture would while recording `episode`. */
function sender(episode: string | null = "e1") {
  return vi.fn((event: SendEvent): Promise<EventSent> =>
    Promise.resolve({ event: { ...event, at_utc_ns: 1, attributes: {}, phase: event.phase ?? "moment", source: "page" }, episode }),
  );
}

describe("MarkBar", () => {
  it("offers each kind, numbered for its key, and marks a moment of the one picked", async () => {
    const send = sender();
    render(MarkBar, { kinds, send });
    const picker = screen.getByRole("combobox", { name: "Kind of event" });
    expect([...picker.querySelectorAll("option")].map((option) => option.textContent)).toEqual([
      "1 · Note",
      "2 · Intervention",
    ]);
    await fireEvent.click(screen.getByRole("button", { name: "Mark Note" }));
    expect(send).toHaveBeenCalledWith({ kind: "note", source: "page" });
    expect(await screen.findByText("Marked the Note")).toBeTruthy();
  });

  it("begins a stretch, then ends the same one, pressing twice", async () => {
    const send = sender();
    render(MarkBar, { kinds, send });
    await fireEvent.change(screen.getByRole("combobox", { name: "Kind of event" }), { target: { value: "intervention" } });
    await fireEvent.click(screen.getByRole("button", { name: "Begin Intervention" }));
    const begun = send.mock.calls[0]?.[0];
    expect(begun).toMatchObject({ kind: "intervention", phase: "begin", source: "page" });
    await fireEvent.click(await screen.findByRole("button", { name: "End Intervention" }));
    expect(send.mock.calls[1]?.[0]).toEqual({ kind: "intervention", phase: "end", id: begun?.id, source: "page" });
    expect(await screen.findByRole("button", { name: "Begin Intervention" })).toBeTruthy();
  });

  it("sends a kind by its number key, but not while typing", async () => {
    const send = sender(null);
    render(MarkBar, { kinds, send });
    await fireEvent.keyDown(window, { key: "1" });
    expect(send).toHaveBeenCalledWith({ kind: "note", source: "page" });
    expect(await screen.findByText("Noted the Note; not recording, so not kept")).toBeTruthy();
    await fireEvent.keyDown(screen.getByRole("combobox", { name: "Kind of event" }), { key: "2" });
    await fireEvent.keyDown(window, { key: "9" });
    expect(send).toHaveBeenCalledOnce();
  });

  it("says why an event was refused", async () => {
    const send = vi.fn(() =>
      Promise.reject(new ApiFailure({ code: "events.unknown_kind", message: "Not a kind.", remedy: "Add it." })),
    );
    render(MarkBar, { kinds, send });
    await fireEvent.click(screen.getByRole("button", { name: "Mark Note" }));
    expect(await screen.findByText("Not a kind. Add it.")).toBeTruthy();
  });
});
