import { expect, test } from "@playwright/test";

import { open, shot, tab } from "./screens";

// What the page shows once the capture process is gone, for a pull
// request's description; `cargo xtask ui screenshots` kills it first.
test("a screenshot of the page without the capture process", async ({ page }) => {
  const nothingLeft = await open(page);
  await expect(page.getByRole("banner").getByRole("button", { name: "Record" })).toBeDisabled();
  await page.waitForTimeout(2_000);
  await shot(page, "without-capture");

  await tab(page, /^System/);
  await shot(page.locator("aside"), "system-without-capture");

  nothingLeft();
});
