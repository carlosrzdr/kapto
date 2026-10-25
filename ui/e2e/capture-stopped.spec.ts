import { expect, test, type Page } from "@playwright/test";

import { requestsLeavingTheMachine } from "./local-only";

const open = (page: Page, tab: RegExp) => page.getByRole("tab", { name: tab }).click();

// Run once the capture process has been stopped, and not started again.
test("with the capture process stopped, the page says why and can't record", async ({ page }) => {
  const elsewhere = requestsLeavingTheMachine(page);
  await page.goto("/");

  const alert = page.getByRole("alert");
  await expect(alert).toContainText("Can't reach the capture process");
  await expect(alert).toContainText("Check that `kapto serve`");
  await expect(alert).not.toContainText("os error");
  const record = page.getByRole("banner").getByRole("button", { name: "Record" });
  await expect(record).toBeDisabled();
  await expect(record).toHaveText("Record");
  // It says why, on hover and to a screen reader, and looks it can't be pressed.
  const why = "Kapto can't reach the capture process, so nothing can be recorded";
  await expect(record).toHaveAccessibleDescription(why);
  await expect(record).toHaveAttribute("title", `Record an episode (R): ${why}`);
  await expect(record).toHaveCSS("cursor", "not-allowed");
  await expect(page.getByText("Capture not reachable")).toBeVisible();
  // Where the robot would be: that it can't be shown, pointing at why,
  // which the page says once.
  const noView = page.getByRole("status").filter({ hasText: "No live view" });
  await expect(noView).toContainText("the message above says why");
  await expect(noView).toContainText("tries again by itself");
  await expect(noView).not.toContainText("Can't reach the capture process");
  await expect(page.getByText(/Check that `kapto serve`/)).toHaveCount(1);
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
  await page.getByRole("listitem").filter({ has: page.getByTitle("Show this episode in full") }).first().getByTitle("Show this episode in full").click();
  await expect(page.getByRole("region", { name: /^Episode / })).toContainText(/ in \d+ files/);

  // The filter lists the rejected episodes only: the flows record some
  // shorter than the 2 s an episode is accepted from.
  const quality = page.getByRole("combobox", { name: "Quality" });
  await quality.selectOption("rejected");
  const listed = page.getByRole("listitem").filter({ has: page.getByTitle("Show this episode in full") });
  await expect(listed.first().or(page.getByText("No rejected episodes."))).toBeVisible();
  // Retried: the list shown before stays until the server answers the filter.
  await expect(listed.filter({ hasNotText: "rejected" })).toHaveCount(0);
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
  // And the 12 it dropped, which the episode itself then counts.
  await page.route(/\/api\/episodes\/[^/?]+$/, async (route) => {
    const response = await route.fetch();
    const body = (await response.json()) as { streams: { dropped_by_us: number }[] };
    body.streams[0]!.dropped_by_us = 12;
    await route.fulfill({ response, json: body });
  });
  const episode = page.getByRole("listitem").filter({ has: page.getByTitle("Show this episode in full") }).first().getByTitle("Show this episode in full");
  await episode.click();
  await episode.click();
  const details = page.getByRole("region", { name: /^Episode / });
  await expect(details).toContainText("rejected: a gate failed · settings 1");
  await expect(details.getByRole("row", { name: /^streams\.complete/ })).toContainText("Kapto dropped 12 messages");
  // The loss is said in the summary, never folded, and opens the streams it is in;
  // how it was recorded stays folded.
  await expect(details.getByRole("definition").filter({ hasText: "12 dropped by Kapto" })).toBeVisible();
  await expect(details.getByRole("table", { name: "Streams" }).getByRole("row").nth(1)).toBeVisible();
  await expect(details.getByText(/^99 % within/)).toBeHidden();
  expect(elsewhere).toEqual([]);
});
