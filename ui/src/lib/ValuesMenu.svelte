<script lang="ts">
  import { DropdownMenu } from "bits-ui";

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
</script>

<div class={["values", { open }]}>
  <DropdownMenu.Root bind:open>
    <DropdownMenu.Trigger class="opener" aria-label="Values shown"><Icon name="list" /></DropdownMenu.Trigger>
    <DropdownMenu.Portal>
      <!-- Drawn over the page, so the panel's edge never cuts it off. -->
      <DropdownMenu.Content class="values-menu" aria-label={heading || "Values shown"} side="left" align="start" sideOffset={4}>
        {#if heading}
          <DropdownMenu.Group aria-label={heading}>
            <DropdownMenu.GroupHeading class="heading">{heading}</DropdownMenu.GroupHeading>
          </DropdownMenu.Group>
        {/if}
        {#each entries as entry, index (index)}
          <!-- Stays open, so several can be shown or hidden in turn. -->
          <DropdownMenu.CheckboxItem
            class={`item swatch series-${entry.style.hue} dash-${entry.style.pattern}`}
            checked={shown[index]}
            closeOnSelect={false}
            onCheckedChange={() => toggle(index)}><span class="check" aria-hidden="true"></span>{entry.label}</DropdownMenu.CheckboxItem
          >
        {/each}
        <DropdownMenu.Item class="item all" onSelect={showAll}>Show all</DropdownMenu.Item>
      </DropdownMenu.Content>
    </DropdownMenu.Portal>
  </DropdownMenu.Root>
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
  .values :global(.opener) {
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
  .values :global(.opener:hover),
  .values.open :global(.opener),
  .values :global(.opener:focus-visible) {
    border-color: var(--text);
  }
  :global(.values-menu) {
    z-index: 40;
    display: flex;
    flex-direction: column;
    min-width: 10rem;
    max-height: min(16rem, var(--bits-dropdown-menu-content-available-height));
    overflow-y: auto;
    padding: var(--space-1);
    background: var(--panel);
    border: 1px solid var(--line);
    border-radius: var(--radius);
    color: var(--text);
    font-family: var(--font-ui);
    font-size: var(--text-xs);
    box-shadow: 0 8px 24px rgb(0 0 0 / 50%);
  }
  :global(.values-menu .heading) {
    margin: 0;
    padding: var(--space-1) var(--space-2);
    color: var(--muted);
  }
  :global(.values-menu .item) {
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
    cursor: pointer;
  }
  :global(.values-menu .item[data-highlighted]) {
    background: var(--raised);
    outline: none;
  }
  :global(.values-menu .check) {
    flex: none;
    width: 0.9rem;
    height: 0.9rem;
    border: 1px solid var(--control-line);
    border-radius: var(--radius);
  }
  :global(.values-menu [aria-checked="true"] .check) {
    border-color: var(--accent);
    background: var(--accent);
    box-shadow: inset 0 0 0 2px var(--panel);
  }
  /* Each value's line as the chart draws it, after its box: its colour and its pattern. */
  :global(.values-menu .swatch::after) {
    content: "";
    flex: none;
    order: -1;
    width: 1.125rem;
    height: 2px;
    background: var(--swatch);
  }
  :global(.values-menu .swatch .check) {
    order: -2;
  }
  :global(.values-menu .dash-1::after) {
    background: repeating-linear-gradient(to right, var(--swatch) 0 6px, transparent 6px 10px);
  }
  :global(.values-menu .dash-2::after) {
    background: repeating-linear-gradient(to right, var(--swatch) 0 2px, transparent 2px 5px);
  }
  :global(.values-menu .dash-3::after) {
    background: repeating-linear-gradient(to right, var(--swatch) 0 10px, transparent 10px 13px, var(--swatch) 13px 15px, transparent 15px 18px);
  }
  :global(.values-menu .series-0) {
    --swatch: var(--series-0);
  }
  :global(.values-menu .series-1) {
    --swatch: var(--series-1);
  }
  :global(.values-menu .series-2) {
    --swatch: var(--series-2);
  }
  :global(.values-menu .series-3) {
    --swatch: var(--series-3);
  }
  :global(.values-menu .all) {
    margin-top: var(--space-1);
    border-top: 1px solid var(--line);
    border-radius: 0;
    color: var(--muted);
  }
</style>
