import { expect, test } from "@playwright/test";

import { requestsLeavingTheMachine } from "./local-only";

// The demo as a new user starts it, with `docker compose --profile demo up`:
// run by `cargo xtask demo-compose`, which then looks for the episode in
// the bucket.
test("the demo started by Compose shows the arm, records an episode and uploads it to RustFS", async ({ page }) => {
  test.setTimeout(180_000);
  const elsewhere = requestsLeavingTheMachine(page);
  await page.goto("/");
  await expect(page.getByText("Live, not recording")).toBeVisible({ timeout: 60_000 });
  // The arm, moving: its frames keep coming.
  const live = page.getByTitle("Live view of the robot");
  await expect(live.getByText(/^Live · \d+ ms$/)).toBeVisible({ timeout: 60_000 });
  const seen = Number(await live.getAttribute("data-frames"));
  await expect.poll(async () => Number(await live.getAttribute("data-frames"))).toBeGreaterThan(seen + 10);

  await page.getByRole("button", { name: "Record", exact: true }).click();
  await expect(page.getByRole("button", { name: "Stop" })).toBeVisible({ timeout: 30_000 });
  await page.waitForTimeout(3_000);
  await page.getByRole("button", { name: "Stop" }).click();
  await expect(page.getByText("Live, not recording")).toBeVisible({ timeout: 30_000 });

  // Verified, then uploaded to the storage beside it.
  await page.getByRole("tab", { name: /^Uploads/ }).click();
  await expect(page.getByRole("tabpanel")).toContainText(/\b1 uploaded\b/, { timeout: 120_000 });
  expect(elsewhere).toEqual([]);
});
