import { fireEvent, render, screen } from "@testing-library/svelte";
import { describe, expect, it } from "vitest";

import type { SettingsOverview } from "./api-types";
import SettingsPanel from "./SettingsPanel.svelte";

const settings: SettingsOverview = {
  file: "/home/u/.config/kapto/kapto.toml",
  file_found: false,
  settings: [
    { key: "server.port", value: "7701", source: "flag --port", default: false, help: "Port of the API and web UI." },
    { key: "log.level", value: "info", source: "default", default: true, help: "How much to log." },
    { key: "worker.max_attempts", value: "3", source: "default", default: true, help: "Starts of a job." },
    { key: "worker.pause_while_recording", value: "false", source: "flag --worker-pause-while-recording", default: false, help: "Wait while recording." },
  ],
};

describe("SettingsPanel", () => {
  it("says how many settings differ from their defaults", () => {
    render(SettingsPanel, { settings });
    expect(screen.getByText("4 settings, 2 changed from the default")).toBeTruthy();
  });

  it("lists every setting with its value and where it came from, changed ones marked", async () => {
    render(SettingsPanel, { settings });
    await fireEvent.click(screen.getByText(/4 settings/));
    const port = screen.getByRole("row", { name: /^server\.port/ });
    expect(port.textContent).toContain("7701");
    expect(port.textContent).toContain("flag --port");
    expect(port.classList.contains("changed")).toBe(true);
    expect(port.getAttribute("title")).toBe("Port of the API and web UI.");
    expect(screen.getByRole("row", { name: /^log\.level/ }).classList.contains("changed")).toBe(false);
    expect(screen.getByText(/not found, so not read/)).toBeTruthy();
  });

  function rows(): string[] {
    return screen
      .getAllByRole("row")
      .slice(1)
      .map((row) => row.querySelector("th")?.textContent ?? "");
  }

  it("shows only the settings whose name, value or source holds every word searched for", async () => {
    render(SettingsPanel, { settings });
    const search = screen.getByRole("searchbox", { name: "Search settings" });
    await fireEvent.input(search, { target: { value: "worker" } });
    expect(rows()).toEqual(["worker.max_attempts", "worker.pause_while_recording"]);
    expect(screen.getByText("2 of 4 settings")).toBeTruthy();
    // Words anywhere, in any case: the value and the source count too.
    await fireEvent.input(search, { target: { value: "WORKER flag" } });
    expect(rows()).toEqual(["worker.pause_while_recording"]);
    await fireEvent.input(search, { target: { value: "" } });
    expect(rows()).toHaveLength(4);
    expect(screen.queryByText(/of 4 settings/)).toBeNull();
  });

  it("shows only the changed settings, and says so when nothing matches", async () => {
    render(SettingsPanel, { settings });
    await fireEvent.click(screen.getByRole("checkbox", { name: "Changed only" }));
    expect(rows()).toEqual(["server.port", "worker.pause_while_recording"]);
    const search = screen.getByRole("searchbox", { name: "Search settings" });
    await fireEvent.input(search, { target: { value: "log" } });
    expect(screen.getByText("No setting matches “log” among those changed.")).toBeTruthy();
    expect(screen.queryByRole("table", { name: "Settings" })).toBeNull();
  });
});
