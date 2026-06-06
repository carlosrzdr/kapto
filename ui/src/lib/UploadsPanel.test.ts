import { fireEvent, render, screen, within } from "@testing-library/svelte";
import { describe, expect, it, vi } from "vitest";

import { ApiFailure } from "./api";
import type { DestinationStatus, UploadsOverview, UploadStatus } from "./api-types";
import type { Measured } from "./uploads";
import UploadsPanel from "./UploadsPanel.svelte";

const MiB = 1024 * 1024;

const archive: DestinationStatus = {
  name: "archive",
  url: "s3://robot-data",
  uploading: 1,
  uploaded: 4,
  failed: 0,
  bytes_sent: 40 * MiB,
  bytes_total: 100 * MiB,
  throttled: 0,
};

const going: UploadStatus = {
  episode: "ep_going",
  destination: "archive",
  state: "uploading",
  key: "arm7/2026-03-25/ep_going",
  bytes_sent: 40 * MiB,
  bytes_total: 100 * MiB,
  throttled: 0,
  started_utc_ns: 2,
  reported_utc_ns: 5e9,
};

const failed: UploadStatus = {
  episode: "ep_failed",
  destination: "nas",
  state: "failed",
  key: "arm7/2026-03-25/ep_failed",
  bytes_sent: 0,
  bytes_total: 10 * MiB,
  throttled: 0,
  started_utc_ns: 1,
  failed_utc_ns: Date.parse("2026-03-25T10:00:00") * 1e6,
  failure_code: "upload.denied",
  failure_message: "the store refused Kapto's credentials (os error 13)",
};

const overview: UploadsOverview = {
  waiting: 2,
  destinations: [archive, { ...archive, name: "nas", url: "file:///srv/kapto", uploading: 0, uploaded: 0, failed: 1 }],
  uploads: [going, failed],
};

const props = (over: Partial<Record<string, unknown>> = {}) => ({
  overview,
  measured: new Map<string, Measured>([["ep_going/archive", { reading: { sent: 40 * MiB, at: 5e9 }, rate: 2 * MiB }]]),
  marker: true,
  onretry: vi.fn(() => Promise.resolve()),
  ontest: vi.fn(() => Promise.resolve({ key: "arm7/.kapto-probe-x", removed: true, took_ms: 42 })),
  ...over,
});

describe("UploadsPanel", () => {
  it("shows each destination's counts and the episodes waiting", () => {
    render(UploadsPanel, props());
    expect(screen.getByText("2 episodes waiting")).toBeTruthy();
    const nas = screen.getByRole("listitem", { name: "nas" });
    expect(nas.textContent).toContain("file:///srv/kapto");
    expect(within(nas).getByText("1 failed").classList.contains("problem")).toBe(true);
    const shown = screen.getByRole("listitem", { name: "archive" });
    expect(shown.textContent).toMatch(/4 uploaded · 1 uploading ·\s+0 failed/);
    expect(within(shown).getByText("0 failed").classList.contains("problem")).toBe(false);
  });

  it("shows an upload going with how fast it goes and how long it has left", () => {
    render(UploadsPanel, props());
    const row = screen.getByRole("listitem", { name: "ep_going to archive" });
    // 60 MiB left at 2 MiB/s.
    expect(row.textContent).toContain("40 MiB of 100 MiB · 2.0 MiB/s · 0:30 left");
  });

  it("shows a failed upload with why, and retries its episode when asked", async () => {
    const onretry = vi.fn(() => Promise.resolve());
    render(UploadsPanel, props({ onretry }));
    const row = screen.getByRole("listitem", { name: "ep_failed to nas" });
    // Without the operating system's error number, which stays in its tooltip.
    expect(row.textContent).toContain("the store refused Kapto's credentials · 2026-03-25 10:00:00");
    expect(row.textContent).toContain("upload.denied");
    await fireEvent.click(within(row).getByRole("button", { name: "Retry now" }));
    expect(onretry).toHaveBeenCalledWith("ep_failed");
  });

  it("says a failed upload retried by hand is queued, rather than when it failed", () => {
    render(UploadsPanel, props({ overview: { ...overview, uploads: [{ ...failed, failed_utc_ns: 0 }] } }));
    const row = screen.getByRole("listitem", { name: "ep_failed to nas" });
    expect(row.textContent).toContain("Queued to be tried again.");
    expect(row.textContent).not.toContain("1970");
    expect(within(row).queryByRole("button")).toBeNull();
  });

  it("says why a retry was refused", async () => {
    const refused = new ApiFailure({
      code: "upload.nothing_to_retry",
      message: "none of ep_failed's uploads failed",
      remedy: "It is already being tried again.",
    });
    render(UploadsPanel, props({ onretry: vi.fn(() => Promise.reject(refused)) }));
    await fireEvent.click(screen.getByRole("button", { name: "Retry now" }));
    expect((await screen.findByRole("alert")).textContent).toContain("none of ep_failed's uploads failed");
  });

  it("tests a destination and says it works", async () => {
    const ontest = vi.fn(() => Promise.resolve({ key: "arm7/.kapto-probe-x", removed: true, took_ms: 42 }));
    render(UploadsPanel, props({ ontest }));
    const nas = screen.getByRole("listitem", { name: "nas" });
    await fireEvent.click(within(nas).getByRole("button", { name: "Test" }));
    expect(ontest).toHaveBeenCalledWith("nas");
    expect((await within(nas).findByRole("status")).textContent).toContain(
      "Works: a probe written, read back and removed in 42 ms",
    );
  });

  it("says where a probe it couldn't remove is left", async () => {
    const ontest = vi.fn(() => Promise.resolve({ key: "arm7/.kapto-probe-x", removed: false, took_ms: 42 }));
    render(UploadsPanel, props({ ontest }));
    await fireEvent.click(within(screen.getByRole("listitem", { name: "nas" })).getByRole("button", { name: "Test" }));
    expect((await screen.findByRole("status")).textContent).toContain("the probe is still at arm7/.kapto-probe-x");
  });

  it("says why a destination's test failed, with its cause code", async () => {
    const denied = new ApiFailure({
      code: "upload.denied",
      message: "the store refused Kapto's credentials",
      remedy: "Check the credentials the destination names.",
    });
    render(UploadsPanel, props({ ontest: vi.fn(() => Promise.reject(denied)) }));
    const archiveItem = screen.getByRole("listitem", { name: "archive" });
    await fireEvent.click(within(archiveItem).getByRole("button", { name: "Test" }));
    expect((await within(archiveItem).findByRole("alert")).textContent).toContain(
      "Check the credentials the destination names.",
    );
    expect(archiveItem.textContent).toContain("upload.denied");
  });

  it("warns when no completeness marker is written", () => {
    render(UploadsPanel, props({ marker: false }));
    expect(screen.getByRole("note").textContent).toContain("no _SUCCESS is written");
  });

  it("says episodes stay here when no destination is configured", () => {
    render(UploadsPanel, props({ overview: { waiting: 0, destinations: [], uploads: [] } }));
    expect(screen.getByText(/No destinations: episodes stay on this machine/)).toBeTruthy();
  });
});
