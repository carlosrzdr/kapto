// The keys operators press most, and when the page listens for them: never
// while they type, nor Space on a focused control, which Space presses.

/** Each key and what it does, as the list of keys shows them. */
export const SHORTCUTS = [
  { keys: "R", does: "Record an episode, or stop recording it" },
  { keys: "Space", does: "Pause the live view, or play back from where it is" },
  { keys: "Esc", does: "Back to now" },
  { keys: "F", does: "Fit the robot in the view" },
  { keys: "1–9", does: "Mark the kind of event with that number" },
  { keys: "?", does: "Show or hide these keys" },
] as const;

/** Whether `target` takes typing: a field or a list. */
function typingIn(target: EventTarget | null): boolean {
  return target instanceof Element && target.closest("input, select, textarea, [contenteditable='true']") !== null;
}

/** Whether `target` is a control Space or Enter already presses. */
function pressable(target: EventTarget | null): boolean {
  return target instanceof Element && target.closest("button, a[href], summary, [role='button']") !== null;
}

/** The key `event` presses for a shortcut, lowercased, or `null` if it isn't one here. */
export function shortcut(event: KeyboardEvent): string | null {
  if (event.ctrlKey || event.metaKey || event.altKey || typingIn(event.target)) {
    return null;
  }
  const key = event.key === " " ? "space" : event.key.toLowerCase();
  if ((key === "space" || key === "enter") && pressable(event.target)) {
    return null;
  }
  return key;
}
