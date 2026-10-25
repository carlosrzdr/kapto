// Using the page's controls as a person does: open a list and choose from
// it, or pick one of a group.

import type { Page } from "@playwright/test";

/** Chooses the option saying `option` in the select named `name`. */
export async function choose(page: Page, name: string, option: string | RegExp): Promise<void> {
  const select = page.getByRole("combobox", { name, exact: true });
  await select.click();
  // Only this select's list, which it names, whatever else offers options.
  const list = page.locator(`[id="${await select.getAttribute("aria-controls")}"]`);
  await list.getByRole("option", { name: option }).click();
}

/** Picks `option` in the group of choices named `name`, e.g. the episodes' quality. */
export async function pick(page: Page, name: string, option: string): Promise<void> {
  await page.getByRole("group", { name }).getByRole("radio", { name: option }).click();
}
