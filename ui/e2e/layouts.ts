import { expect, type Page } from "@playwright/test";

/** A panel of a layout, as the API takes and gives it. */
export interface Panel {
  kind: string;
  height: number;
  fields?: string[];
  stream?: string | null;
  title?: string | null;
}

/** A column of a layout. */
export interface Column {
  width: number;
  panels: Panel[];
}

/** Where the page remembers which layout it shows for the demo's package. */
const SHOWN = "kapto.layout-shown.arm7";

/** Saves `columns` as the demo package's layout `name` and shows it, as if picked from the list; answers with its id. */
export async function showLayout(page: Page, name: string, columns: Column[]): Promise<string> {
  const response = await page.request.post("/api/layouts", { data: { name, columns } });
  expect(response.status(), await response.text()).toBe(200);
  const { id } = (await response.json()) as { id: string };
  await page.evaluate(([key, shown]) => localStorage.setItem(key!, shown!), [SHOWN, id]);
  await page.reload();
  return id;
}

/** The named layouts as the server holds them now. */
export async function savedLayouts(page: Page): Promise<{ id: string; name: string; layout: { columns: Column[] } }[]> {
  const response = await page.request.get("/api/layouts");
  return ((await response.json()) as { layouts: { id: string; name: string; layout: { columns: Column[] } }[] }).layouts;
}

/** The columns of the named layout `id`, as the server holds them now. */
export async function savedLayout(page: Page, id: string): Promise<Column[]> {
  return (await savedLayouts(page)).find((each) => each.id === id)?.layout.columns ?? [];
}

/** Deletes every named layout and forgets which one the page showed, as the flows found it. */
export async function forgetLayouts(page: Page): Promise<void> {
  for (const { id } of await savedLayouts(page)) {
    expect((await page.request.delete(`/api/layouts/${encodeURIComponent(id)}`)).status()).toBe(200);
  }
  await page.evaluate((key) => localStorage.removeItem(key), SHOWN);
}
