import type { Page } from "@playwright/test";

/**
 * Collects every request the page and its frames make to anywhere but this
 * machine. Kapto's pages load nothing from elsewhere, so it stays empty.
 */
export function requestsLeavingTheMachine(page: Page): string[] {
  const elsewhere: string[] = [];
  page.on("request", (request) => {
    const url = new URL(request.url());
    const local =
      url.protocol === "data:" ||
      url.protocol === "blob:" ||
      ["localhost", "127.0.0.1", "[::1]"].includes(url.hostname);
    if (!local) {
      elsewhere.push(request.url());
    }
  });
  return elsewhere;
}
