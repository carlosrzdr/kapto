// Using the page's controls from component tests as a person would: open a
// list, see what it offers, choose from it.

import { fireEvent, screen, waitFor, within } from "@testing-library/svelte";

/** Opens the list of the select named `name`, as the keyboard does. */
export async function openList(name: string): Promise<HTMLElement[]> {
  const trigger = screen.getByRole("combobox", { name });
  await fireEvent.keyDown(trigger, { key: "Enter" });
  // Only this select's list, which its control names, whatever else is open.
  const list = await waitFor(() => {
    const found = document.getElementById(trigger.getAttribute("aria-controls") ?? "");
    if (!found) {
      throw new Error(`the list of ${name} didn't open`);
    }
    return found;
  });
  return within(list).findAllByRole("option");
}

/** What each option of the open list says, and whether it can be chosen. */
export function offered(options: HTMLElement[]): [string, boolean][] {
  return options.map((option) => [
    option.textContent?.replace(/^✓/, "").replace(/\s+/g, " ").trim() ?? "",
    option.getAttribute("aria-disabled") !== "true",
  ]);
}

/** Chooses `label` in the select named `name`. */
export async function choose(name: string, label: string | RegExp): Promise<void> {
  const options = await openList(name);
  const option = options.find((each) =>
    typeof label === "string" ? each.textContent?.includes(label) : label.test(each.textContent ?? ""),
  );
  if (!option) {
    throw new Error(`no option ${String(label)} in ${name}`);
  }
  await fireEvent.pointerUp(option);
  await fireEvent.click(option);
}

/** Chooses the option whose value is `value` in the select named `name`. */
export async function chooseValue(name: string, value: string): Promise<void> {
  const option = (await openList(name)).find((each) => each.dataset["value"] === value);
  if (!option) {
    throw new Error(`no option of value ${value} in ${name}`);
  }
  await fireEvent.pointerUp(option);
  await fireEvent.click(option);
}
