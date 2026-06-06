import { expect, type Locator, type Page } from "@playwright/test";

import { requestsLeavingTheMachine } from "./local-only";

/** Where `cargo xtask ui screenshots` collects them. */
const FOLDER = process.env.KAPTO_SCREENSHOTS ?? "screenshots";

/** The window every screenshot is taken in: a laptop screen. */
export const VIEWPORT = { width: 1400, height: 900 };

/** Saves `what` (the whole page, or one part of it) as `<name>.png`. */
export async function shot(what: Page | Locator, name: string): Promise<void> {
  await what.screenshot({ path: `${FOLDER}/${name}.png`, animations: "disabled" });
}

/**
 * Saves `part` of the side panel as `<name>.png` whole: the panel scrolls
 * what is taller than the window, which would cut a long part off, so it
 * is let grow for the screenshot and put back after.
 */
export async function shotWhole(page: Page, part: Locator, name: string): Promise<void> {
  const aside = page.locator("aside");
  await aside.evaluate((panel) => (panel.style.overflowY = "visible"));
  await shot(part, name);
  await aside.evaluate((panel) => (panel.style.overflowY = ""));
}

/**
 * Opens the page and waits until it shows the robot live, failing if
 * anything is asked of another machine while the screenshots are taken.
 */
export async function open(page: Page): Promise<() => void> {
  const elsewhere = requestsLeavingTheMachine(page);
  await page.setViewportSize(VIEWPORT);
  await page.goto("/");
  return () => expect(elsewhere).toEqual([]);
}

/**
 * Hides or shows the live viewer. The browser here draws it in software,
 * which takes so much of the page's time that every other step, a click
 * as much as a screenshot, can wait for minutes; hidden, Chrome stops
 * drawing it. It is shown only for the screenshots that include it.
 */
export async function viewer(page: Page, shown: boolean): Promise<void> {
  await page
    .getByTitle("Live view of the robot")
    .evaluate((frame, display) => ((frame as HTMLElement).style.display = display), shown ? "" : "none");
}

/** Selects a tab by the start of its name. */
export async function tab(page: Page, name: RegExp): Promise<void> {
  await page.getByRole("tab", { name }).click();
}
