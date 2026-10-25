import { fireEvent, render, screen } from "@testing-library/svelte";
import { describe, expect, it, vi } from "vitest";

import Tabs from "./Tabs.svelte";

const tabs = [
  { id: "live", label: "Live", icon: "pulse" as const },
  { id: "episodes", label: "Episodes", icon: "list" as const },
  { id: "uploads", label: "Uploads", icon: "upload" as const, alert: "danger" as const },
  { id: "system", label: "System", icon: "chip" as const, alert: "warning" as const },
];

describe("Tabs", () => {
  it("marks the selected tab and asks for another when it is clicked", async () => {
    const onselect = vi.fn();
    render(Tabs, { tabs, selected: "live", onselect });
    expect(screen.getByRole("tab", { name: "Live" }).getAttribute("aria-selected")).toBe("true");
    expect(screen.getByRole("tab", { name: "Episodes" }).getAttribute("aria-selected")).toBe("false");
    await fireEvent.click(screen.getByRole("tab", { name: "Episodes" }));
    expect(onselect).toHaveBeenCalledWith("episodes");
  });

  it("moves with the arrow keys, round from the last to the first", async () => {
    const onselect = vi.fn();
    render(Tabs, { tabs, selected: "system", onselect });
    await fireEvent.keyDown(screen.getByRole("tab", { name: "System needs attention" }), { key: "ArrowRight" });
    expect(onselect).toHaveBeenCalledWith("live");
  });

  it("marks a tab that holds a problem, in words too, not by colour alone", () => {
    render(Tabs, { tabs, selected: "live", onselect: vi.fn() });
    expect(screen.getByRole("tab", { name: "System needs attention" }).classList.contains("alert")).toBe(true);
    expect(screen.getByRole("tab", { name: "Live" }).classList.contains("alert")).toBe(false);
  });

  it("marks it in the colour of how badly it needs attention", () => {
    render(Tabs, { tabs, selected: "live", onselect: vi.fn() });
    const mark = (name: string) => screen.getByRole("tab", { name }).querySelector(".attention");
    expect(mark("Uploads needs attention")?.classList.contains("danger")).toBe(true);
    expect(mark("System needs attention")?.classList.contains("warning")).toBe(true);
  });

  it("folds to a strip of icons, each opening its tab, still marked where it needs attention", async () => {
    const onselect = vi.fn();
    render(Tabs, { tabs, selected: "live", onselect, folded: true, onfold: vi.fn() });
    expect(screen.queryByRole("tab")).toBeNull();
    expect(screen.getByRole("button", { name: "Uploads needs attention" }).querySelector(".attention.danger")).not.toBeNull();
    await fireEvent.click(screen.getByRole("button", { name: "Episodes" }));
    expect(onselect).toHaveBeenCalledWith("episodes");
  });

  it("asks to fold from its edge, and to open again once folded", async () => {
    const onfold = vi.fn();
    const shown = render(Tabs, { tabs, selected: "live", onselect: vi.fn(), onfold });
    await fireEvent.click(screen.getByRole("button", { name: "Fold the side panel" }));
    expect(onfold).toHaveBeenLastCalledWith(true);
    await shown.rerender({ tabs, selected: "live", onselect: vi.fn(), onfold, folded: true });
    const open = screen.getByRole("button", { name: "Open the side panel" });
    expect(open.getAttribute("aria-expanded")).toBe("false");
    await fireEvent.click(open);
    expect(onfold).toHaveBeenLastCalledWith(false);
  });

  it("has no fold button where the panel can't fold", () => {
    render(Tabs, { tabs, selected: "live", onselect: vi.fn() });
    expect(screen.queryByRole("button", { name: "Fold the side panel" })).toBeNull();
  });
});
