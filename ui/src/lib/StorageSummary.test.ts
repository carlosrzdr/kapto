import { render, screen } from "@testing-library/svelte";
import { describe, expect, it } from "vitest";

import type { Storage } from "./api-types";
import StorageSummary from "./StorageSummary.svelte";

const GiB = 1024 ** 3;

const storage: Storage = {
  data_dir: "/var/lib/kapto",
  total_bytes: 400 * GiB,
  free_bytes: 120 * GiB,
  episodes: 12,
  episodes_bytes: 3.4 * GiB,
  staging_bytes: 0,
  warn_below_bytes: 10 * GiB,
  stop_below_bytes: 2 * GiB,
};

describe("StorageSummary", () => {
  it("shows the free space, with the episodes and the folder behind a disclosure, closed while there is room", () => {
    render(StorageSummary, { storage, writing: null });
    expect(screen.getByText("120 GiB free of 400 GiB")).toBeTruthy();
    const details = screen.getByText("Episodes and folder").closest("details");
    expect(details?.open).toBe(false);
    expect(details?.textContent).toContain("12 · 3.4 GiB");
    expect(details?.textContent).toContain("/var/lib/kapto");
    expect(screen.queryByText("Writing")).toBeNull();
  });

  it("while recording, shows how fast it writes and how long the disk lasts at that rate", () => {
    render(StorageSummary, { storage, writing: 2 * 1024 * 1024 });
    expect(screen.getByText(/^2\.0 MiB\/s/)).toBeTruthy();
    // 120 GiB, less the 2 GiB recording stops at, at 2 MiB/s: 60 416 s.
    expect(screen.getByText("· 16:46:56 left at this rate")).toBeTruthy();
  });

  it("marks a disk below its warning limit, and says where recording stops", () => {
    render(StorageSummary, { storage: { ...storage, free_bytes: 9 * GiB }, writing: null });
    expect(screen.getByText("9.0 GiB free of 400 GiB").classList.contains("problem")).toBe(true);
    expect(screen.getByText("Recording stops below 2.0 GiB free")).toBeTruthy();
    // What takes the room, and where, shown at once.
    expect(screen.getByText("Episodes and folder").closest("details")?.open).toBe(true);
  });

  it("doesn't mark a disk above its warning limit, however full in share", () => {
    render(StorageSummary, { storage: { ...storage, free_bytes: 11 * GiB }, writing: null });
    expect(screen.getByText("11 GiB free of 400 GiB").classList.contains("problem")).toBe(false);
    expect(screen.queryByText(/Recording stops/)).toBeNull();
  });

  it("says so while the storage isn't known", () => {
    render(StorageSummary, { storage: null, writing: null });
    expect(screen.getByText("Not known yet.")).toBeTruthy();
  });
});
