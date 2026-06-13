import { expect, type Page } from "@playwright/test";

/**
 * Fails, naming each, if anything in the side panel reaches past its right
 * edge: text the panel would cut off, as it scrolls only down.
 */
export async function fitsPanel(page: Page, where: string): Promise<void> {
  const beyond = await page.locator("aside").evaluate((panel) => {
    // Its inner edge: once it scrolls, its scrollbar covers what lies beyond.
    const edge = panel.getBoundingClientRect().left + panel.clientLeft + panel.clientWidth;
    const room = panel.clientWidth - panel.scrollWidth;
    const found: string[] = [];
    for (const element of panel.querySelectorAll<HTMLElement>("*")) {
      const box = element.getBoundingClientRect();
      if (box.width === 0 || box.height === 0) {
        continue;
      }
      const over = box.right - edge;
      if (over > 1) {
        const text = (element.textContent ?? "").trim().replace(/\s+/g, " ").slice(0, 40);
        found.push(`<${element.tagName.toLowerCase()} class="${element.className}"> ${Math.round(over)}px past: "${text}"`);
      }
    }
    if (room < 0) {
      found.unshift(`the panel's content is ${-room}px wider than the panel`);
    }
    return found;
  });
  expect(beyond, `the side panel cuts off text ${where}`).toEqual([]);
}
