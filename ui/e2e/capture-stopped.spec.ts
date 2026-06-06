import { expect, test, type Page } from "@playwright/test";

import { requestsLeavingTheMachine } from "./local-only";

const open = (page: Page, tab: RegExp) => page.getByRole("tab", { name: tab }).click();

// Run once the capture process has been stopped, and not started again.
test("with the capture process stopped, the page says why and can't record", async ({ page }) => {
  const elsewhere = requestsLeavingTheMachine(page);
  await page.goto("/");

  const alert = page.getByRole("alert");
  await expect(alert).toContainText("Can't reach the capture process");
  await expect(alert).toContainText("Check that `kapto capture` is running");
  await expect(alert).not.toContainText("os error");
  const record = page.getByRole("banner").getByRole("button", { name: "Record" });
  await expect(record).toBeDisabled();
  await expect(record).toHaveText("Record");
  await expect(page.getByText("Capture not reachable")).toBeVisible();
  // Where the robot would be: why it isn't, and what to do.
  const noView = page.getByRole("status").filter({ hasText: "No live view" });
  await expect(noView).toContainText("Can't reach the capture process");
  await expect(noView).toContainText("tries again by itself");
  await expect(page.getByText("Not known while Kapto can't reach the capture process.")).toBeVisible();
  // The disk is still known without capture.
  await expect(page.getByText(/ free of /)).toBeVisible();

  // Killed and not started again for an hour: the system tab says so.
  const system = page.getByRole("tab", { name: /^System/ });
  await expect(system).toHaveClass(/alert/);
  await system.click();
  await expect(page.getByRole("row", { name: /^capture restarting in / })).toBeVisible();
  await expect(page.getByText("capture ended: signal: 9 (SIGKILL)")).toBeVisible();
  await expect(page.getByRole("row", { name: /^viz-bridge up / })).toBeVisible();
  await expect(page.getByText(/ changed from the default$/)).toBeVisible();

  // The robot is still known, but not where its joints are.
  await open(page, /^Robot/);
  await expect(page.getByRole("table", { name: "Declared streams" })).toContainText("arm_joints");
  const first = page.getByRole("table", { name: "Joints" }).getByRole("row", { name: /^joint_1 / });
  await expect(first.getByRole("cell").first()).toHaveText("–");

  // The episodes too.
  await open(page, /^Episodes/);
  await page.getByRole("listitem").filter({ hasText: "messages" }).first().getByTitle("Show this episode in full").click();
  await expect(page.getByRole("region", { name: /^Episode / })).toContainText(/ in \d+ files/);

  // None of the demo's episodes was rejected.
  const quality = page.getByRole("combobox", { name: "Quality" });
  await quality.selectOption("rejected");
  await expect(page.getByText("No rejected episodes.")).toBeVisible();
  await quality.selectOption("all");

  // One a gate rejected says so, with what the gate found. The demo's
  // recordings are complete, so that answer is given here.
  await page.route("**/api/episodes/*/qa", (route) =>
    route.fulfill({
      json: {
        accepted: false,
        params_version: "1",
        computed_utc_ns: 1_772_700_500_000_000_000,
        checks: [
          {
            check: "streams.complete",
            version: 1,
            kind: "gate",
            verdict: "failed",
            message: "Kapto dropped 12 messages: arm_joints (12 of 2000 received)",
            values: { dropped_by_us: 12 },
            ranges: [],
          },
        ],
      },
    }),
  );
  const episode = page.getByRole("listitem").filter({ hasText: "messages" }).first().getByTitle("Show this episode in full");
  await episode.click();
  await episode.click();
  const details = page.getByRole("region", { name: /^Episode / });
  await expect(details).toContainText("rejected: a gate failed · settings 1");
  await expect(details.getByRole("row", { name: /^streams\.complete/ })).toContainText("Kapto dropped 12 messages");
  expect(elsewhere).toEqual([]);
});
