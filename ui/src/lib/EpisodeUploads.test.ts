import { fireEvent, render, screen, within } from "@testing-library/svelte";
import { describe, expect, it, vi } from "vitest";

import type { EpisodeUpload, EpisodeUploads as Uploads } from "./api-types";
import EpisodeUploads from "./EpisodeUploads.svelte";

const MiB = 1024 * 1024;

const uploaded: EpisodeUpload = {
  episode: "ep1",
  destination: "archive",
  state: "uploaded",
  key: "fleet-a/arm7/2026-03-25/ep1",
  bytes_sent: 12 * MiB,
  bytes_total: 12 * MiB,
  throttled: 0,
  started_utc_ns: 1,
  uploaded_utc_ns: Date.parse("2026-03-25T10:00:00") * 1e6,
  objects: [
    { path: "recording.mcap", key: "fleet-a/arm7/2026-03-25/ep1/recording.mcap", bytes: 12 * MiB, sha256: "a".repeat(64) },
  ],
};

const failed: EpisodeUpload = {
  ...uploaded,
  destination: "nas",
  state: "failed",
  bytes_sent: 0,
  uploaded_utc_ns: null,
  failure_code: "upload.denied",
  failure_message: "the store refused Kapto's credentials (os error 13)",
  objects: [],
};

const here: Uploads = { stored_here: true, uploads: [uploaded, failed] };
const destinations = [
  { name: "archive", url: "s3://robot-data/" },
  { name: "nas", url: "file:///srv/kapto" },
];

describe("EpisodeUploads", () => {
  it("shows where each upload went, and each object it holds there", () => {
    render(EpisodeUploads, { uploads: here, destinations });
    const archive = screen.getByRole("region", { name: "Upload to archive" });
    expect(archive.textContent).toContain("uploaded 2026-03-25 10:00:00");
    expect(archive.textContent).toContain("s3://robot-data/fleet-a/arm7/2026-03-25/ep1");
    const objects = within(archive).getByRole("table", { name: "Objects at archive" });
    expect(within(objects).getByRole("row", { name: /recording\.mcap/ }).textContent).toContain("12 MiB");
  });

  it("says why an upload failed, with its cause code", () => {
    render(EpisodeUploads, { uploads: here, destinations });
    const nas = screen.getByRole("region", { name: "Upload to nas" });
    expect(nas.textContent).toContain("failed: the store refused Kapto's credentials upload.denied");
  });

  it("folds each object and its URI behind how many files there are, never the failure", () => {
    Object.defineProperty(navigator, "clipboard", { value: { writeText: vi.fn() }, configurable: true });
    render(EpisodeUploads, { uploads: here, destinations });
    const archive = screen.getByRole("region", { name: "Upload to archive" });
    const objects = within(archive).getByRole("table", { name: "Objects at archive" }).closest("details");
    expect(objects?.open).toBe(false);
    expect(objects?.querySelector("summary")?.textContent).toMatch(/1\s+file · 12 MiB/);
    // The folder's URI stays out, to copy without opening anything.
    expect(within(archive).getByRole("button", { name: "Copy folder URI" }).closest("details")).toBeNull();
    const failure = screen.getByText(/failed: the store refused/);
    expect(failure.closest("details")).toBeNull();
    expect(failure.classList.contains("danger")).toBe(true);
  });

  it("copies an object's URI", async () => {
    const writeText = vi.fn(() => Promise.resolve());
    Object.defineProperty(navigator, "clipboard", { value: { writeText }, configurable: true });
    render(EpisodeUploads, { uploads: here, destinations });
    await fireEvent.click(screen.getByRole("button", { name: "Copy URI" }));
    expect(writeText).toHaveBeenCalledWith("s3://robot-data/fleet-a/arm7/2026-03-25/ep1/recording.mcap");
    expect(await screen.findByRole("button", { name: "Copied" })).toBeTruthy();
  });

  it("copies the URI of the episode's folder at a destination", async () => {
    const writeText = vi.fn(() => Promise.resolve());
    Object.defineProperty(navigator, "clipboard", { value: { writeText }, configurable: true });
    render(EpisodeUploads, { uploads: here, destinations });
    const archive = screen.getByRole("region", { name: "Upload to archive" });
    await fireEvent.click(within(archive).getByRole("button", { name: "Copy folder URI" }));
    expect(writeText).toHaveBeenCalledWith("s3://robot-data/fleet-a/arm7/2026-03-25/ep1");
  });

  it("shows only the key of an upload to a destination no longer configured", () => {
    render(EpisodeUploads, { uploads: here, destinations: [] });
    expect(screen.getByRole("region", { name: "Upload to archive" }).textContent).toContain(
      "fleet-a/arm7/2026-03-25/ep1",
    );
    expect(screen.queryByText(/s3:\/\//)).toBeNull();
  });

  it("says an episode deleted here is stored only remotely, when, why and where", () => {
    render(EpisodeUploads, {
      uploads: {
        stored_here: false,
        deletion: {
          deleted_utc_ns: Date.parse("2026-03-25T11:00:00") * 1e6,
          reason: "disk_low",
          deleted_by: "worker",
          destinations: ["archive"],
        },
        uploads: [uploaded],
      },
      destinations,
    });
    expect(screen.getByText(/^Stored only remotely/).textContent).toMatch(
      /deleted here 2026-03-25 11:00:00, the disk ran low, with verified copies at archive\./,
    );
  });

  it("says when an episode isn't uploaded anywhere", () => {
    render(EpisodeUploads, { uploads: { stored_here: true, uploads: [] }, destinations });
    expect(screen.getByText("Not uploaded anywhere.")).toBeTruthy();
  });
});
