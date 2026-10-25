<script lang="ts">
  import { tick } from "svelte";

  import Icon from "./Icon.svelte";
  import type { SeriesStyle } from "./theme";

  interface Props {
    /** What the values share, said over them. */
    heading: string;
    /** Each value: its label, drawn as its line is. */
    entries: { label: string; style: SeriesStyle }[];
    /** Each value, by place: whether it is shown. */
    shown: boolean[];
    /** Shows the value at a place beside the others, or hides it. */
    toggle: (place: number) => void;
    /** Shows every value. */
    showAll: () => void;
  }

  let { heading, entries, shown, toggle, showAll }: Props = $props();

  let open = $state(false);
  let opener = $state<HTMLButtonElement | null>(null);
  let menu = $state<HTMLElement | null>(null);
  const id = $props.id();
  const items = () => Array.from(menu?.querySelectorAll<HTMLElement>("[role^='menuitem']") ?? []);

  async function openMenu() {
    open = true;
    await tick();
    // No taller than the panel below its button, as the panel cuts off what goes past it.
    const panel = opener?.closest("section");
    if (menu && panel && opener) {
      const room = panel.getBoundingClientRect().bottom - opener.getBoundingClientRect().bottom - 8;
      menu.style.maxHeight = `${Math.min(Math.max(room, 64), 256)}px`;
    }
    items()[0]?.focus();
  }

  function close(refocus: boolean) {
    open = false;
    if (refocus) {
      opener?.focus();
    }
  }

  /** Arrows, Home and End move between the items; Escape closes the menu. */
  function key(event: KeyboardEvent) {
    const all = items();
    const at = all.indexOf(document.activeElement as HTMLElement);
    const to: Record<string, number> = { ArrowDown: at + 1, ArrowUp: at - 1, Home: 0, End: all.length - 1 };
    if (event.key === "Escape") {
      // Closes the menu only, not anything the page does on Escape.
      event.stopPropagation();
      close(true);
    } else if (event.key === "Tab") {
      // From its button, so the focus moves on from there rather than from an item taken away.
      close(true);
    } else if (event.key in to) {
      event.preventDefault();
      all[((to[event.key] ?? 0) + all.length) % all.length]?.focus();
    }
  }

  /** Closes the menu once something outside it is pressed. */
  function pressedSomewhere(event: PointerEvent) {
    if (open && !(event.target instanceof Node && (menu?.contains(event.target) || opener?.contains(event.target)))) {
      close(false);
    }
  }
</script>

<svelte:window onpointerdown={pressedSomewhere} />

<div class={["values", { open }]}>
  <button
    type="button"
    class="opener"
    aria-label="Values shown"
    aria-haspopup="menu"
    aria-expanded={open}
    aria-controls={id}
    bind:this={opener}
    onclick={() => (open ? close(false) : openMenu())}><Icon name="list" /></button
  >
  {#if open}
    <div class="menu" {id} role="menu" aria-label={heading || "Values shown"} tabindex="-1" bind:this={menu} onkeydown={key}>
      {#if heading}
        <p class="heading">{heading}</p>
      {/if}
      {#each entries as entry, index (index)}
        <button
          type="button"
          role="menuitemcheckbox"
          aria-checked={shown[index]}
          tabindex="-1"
          class={`swatch series-${entry.style.hue} dash-${entry.style.pattern}`}
          onclick={() => toggle(index)}><span class="check" aria-hidden="true"></span>{entry.label}</button
        >
      {/each}
      <button type="button" role="menuitem" tabindex="-1" class="all" onclick={showAll}>Show all</button>
    </div>
  {/if}
</div>

<style>
  /* In the panel's corner while the pointer is over it or the focus is in it, and while open. */
  .values {
    position: absolute;
    top: var(--space-1);
    right: var(--space-1);
    z-index: 2;
    opacity: 0;
  }
  :global(:hover) > .values,
  :global(:focus-within) > .values,
  .values.open {
    opacity: 1;
  }
  /* Nothing hovers on a touch screen: always there. */
  @media (hover: none) {
    .values {
      opacity: 1;
    }
  }
  .opener {
    display: grid;
    place-items: center;
    width: var(--control-height);
    height: var(--control-height);
    padding: 0;
    border: 1px solid var(--control-line);
    border-radius: var(--radius);
    background: var(--panel);
    color: var(--text);
    font-size: var(--text-md);
  }
  .opener:hover,
  .open .opener {
    border-color: var(--text);
  }
  .menu {
    position: absolute;
    top: calc(100% + var(--space-1));
    right: 0;
    display: flex;
    flex-direction: column;
    min-width: 10rem;
    overflow-y: auto;
    padding: var(--space-1);
    background: var(--panel);
    border: 1px solid var(--line);
    border-radius: var(--radius);
    font-size: var(--text-xs);
  }
  .heading {
    margin: 0;
    padding: var(--space-1) var(--space-2);
    color: var(--muted);
  }
  .menu button {
    display: flex;
    flex: none;
    align-items: center;
    gap: var(--space-2);
    min-height: var(--control-height);
    padding: 0 var(--space-2);
    border: 0;
    border-radius: var(--radius);
    background: transparent;
    color: var(--text);
    font: inherit;
    text-align: left;
  }
  .menu button:hover,
  .menu button:focus-visible {
    background: var(--raised);
  }
  .check {
    flex: none;
    width: 0.9rem;
    height: 0.9rem;
    border: 1px solid var(--control-line);
    border-radius: var(--radius);
  }
  [aria-checked="true"] .check {
    border-color: var(--accent);
    background: var(--accent);
    box-shadow: inset 0 0 0 2px var(--panel);
  }
  /* Each value's line as the chart draws it, after its box: its colour and its pattern. */
  .swatch::after {
    content: "";
    flex: none;
    order: -1;
    width: 1.125rem;
    height: 2px;
    background: var(--swatch);
  }
  .swatch .check {
    order: -2;
  }
  .dash-1::after {
    background: repeating-linear-gradient(to right, var(--swatch) 0 6px, transparent 6px 10px);
  }
  .dash-2::after {
    background: repeating-linear-gradient(to right, var(--swatch) 0 2px, transparent 2px 5px);
  }
  .dash-3::after {
    background: repeating-linear-gradient(to right, var(--swatch) 0 10px, transparent 10px 13px, var(--swatch) 13px 15px, transparent 15px 18px);
  }
  .series-0 {
    --swatch: var(--series-0);
  }
  .series-1 {
    --swatch: var(--series-1);
  }
  .series-2 {
    --swatch: var(--series-2);
  }
  .series-3 {
    --swatch: var(--series-3);
  }
  .menu .all {
    margin-top: var(--space-1);
    border-top: 1px solid var(--line);
    border-radius: 0;
    color: var(--muted);
  }
</style>
