import { fireEvent, render, screen } from "@testing-library/svelte";
import { describe, expect, it, vi } from "vitest";

import Tabs from "./Tabs.svelte";

const tabs = [
  { id: "live", label: "Live" },
  { id: "episodes", label: "Episodes" },
  { id: "uploads", label: "Uploads", alert: "danger" as const },
  { id: "system", label: "System", alert: "warning" as const },
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
});
