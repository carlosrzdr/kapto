<script lang="ts">
  import { DropdownMenu } from "bits-ui";

  import Icon from "./Icon.svelte";
  import { DEFAULT, type PackageLayouts } from "./viewer/layout-list.svelte";

  interface Props {
    /** The package's layouts, the one shown and what the list last did. */
    layouts: PackageLayouts;
  }

  let { layouts }: Props = $props();

  let open = $state(false);
  const shown = $derived(layouts.shown);
  const isDefault = $derived(shown?.id === DEFAULT);

</script>

<div class="layouts" role="group" aria-label="Layouts">
  <DropdownMenu.Root bind:open onOpenChange={(now) => now && void layouts.refresh()}>
    <DropdownMenu.Trigger class="layouts-opener" aria-label={`Layout: ${shown?.name ?? "loading"}`} disabled={!shown}>
      <span class="shown">{shown?.name ?? "Layout"}</span>
      <Icon name="chevron" />
    </DropdownMenu.Trigger>
    <DropdownMenu.Portal>
      <DropdownMenu.Content class="layouts-menu" aria-label="Layouts" align="end" sideOffset={4}>
        <DropdownMenu.RadioGroup value={layouts.shownId ?? ""} onValueChange={(id) => layouts.pick(id)} aria-label="Shown">
          {#each layouts.offers as offer (offer.id)}
            <DropdownMenu.RadioItem class="item" value={offer.id}>
              {#snippet children({ checked })}
                <span class="mark" aria-hidden="true">{checked ? "✓" : ""}</span>{offer.name}
              {/snippet}
            </DropdownMenu.RadioItem>
          {/each}
        </DropdownMenu.RadioGroup>
        <DropdownMenu.Separator class="rule" />
        <DropdownMenu.Item class="item" disabled={isDefault} onSelect={() => void layouts.saveAsDefault()}>Save as Default</DropdownMenu.Item>
      </DropdownMenu.Content>
    </DropdownMenu.Portal>
  </DropdownMenu.Root>
  {#if layouts.said}
    <div class="box">
      <div class="said">
        <p class:problem={layouts.failed} role={layouts.failed ? "alert" : "status"}>{layouts.said}</p>
        <button type="button" class="dismiss" aria-label="Dismiss" onclick={() => layouts.dismiss()}>×</button>
      </div>
    </div>
  {/if}
</div>

<style>
  .layouts {
    position: relative;
    display: flex;
  }
  .layouts :global(.layouts-opener) {
    display: inline-flex;
    gap: var(--space-2);
    align-items: center;
    max-width: 14rem;
    padding: 0 var(--space-2) 0 var(--space-3);
    border: 1px solid var(--control-line);
    border-radius: var(--radius);
    background: transparent;
    color: var(--text);
    font: inherit;
    font-size: var(--text-sm);
    cursor: pointer;
  }
  .layouts :global(.layouts-opener:hover:not(:disabled)),
  .layouts :global(.layouts-opener:focus-visible) {
    border-color: var(--text);
  }
  /* Its menu open: lit, as every button that opens a panel. */
  .layouts :global(.layouts-opener[data-state="open"]) {
    border-color: var(--text);
    background: var(--raised);
  }
  .layouts :global(.layouts-opener:disabled) {
    color: var(--muted);
    cursor: default;
  }
  /* The chevron points right elsewhere; here, down to the list. */
  .layouts :global(.layouts-opener .icon) {
    transform: rotate(90deg);
  }
  .shown {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  /* The menu is drawn outside the page's flow, so its rules are global. */
  :global(.layouts-menu) {
    z-index: 40;
    display: flex;
    flex-direction: column;
    min-width: 12rem;
    max-height: min(24rem, var(--bits-dropdown-menu-content-available-height));
    overflow-y: auto;
    padding: var(--space-1);
    border: 1px solid var(--line);
    border-radius: var(--radius);
    background: var(--panel);
    color: var(--text);
    font-family: var(--font-ui);
    font-size: var(--text-sm);
    box-shadow: 0 8px 24px rgb(0 0 0 / 50%);
  }
  :global(.layouts-menu .item) {
    display: flex;
    flex: none;
    align-items: center;
    gap: var(--space-2);
    min-height: var(--control-height);
    padding: 0 var(--space-2);
    border-radius: var(--radius);
    cursor: pointer;
  }
  :global(.layouts-menu .item[data-highlighted]) {
    background: var(--raised);
    outline: none;
  }
  :global(.layouts-menu .item[aria-checked="true"]) {
    color: var(--accent);
  }
  :global(.layouts-menu .item[data-disabled]) {
    color: var(--muted);
    cursor: not-allowed;
  }
  :global(.layouts-menu .rule) {
    height: 1px;
    margin: var(--space-1) 0;
    background: var(--line);
  }
  .mark {
    width: 1ch;
    flex: none;
  }
  /* Under the list, over the panels while it tells, never moving them. */
  .box {
    position: absolute;
    top: calc(100% + var(--space-1));
    right: 0;
    z-index: 3;
    display: flex;
    flex-direction: column;
    gap: var(--space-2);
    width: min(24rem, 90vw);
    padding: var(--space-2);
    border: 1px solid var(--line);
    border-radius: var(--radius);
    background: var(--panel);
    font-size: var(--text-sm);
    box-sizing: border-box;
  }
  p {
    margin: 0;
  }
  p[role="status"] {
    color: var(--muted);
  }
  .problem {
    color: var(--warning);
  }
  button {
    height: var(--control-height);
    padding: 0 var(--space-3);
    border: 1px solid var(--control-line);
    border-radius: var(--radius);
    background: transparent;
    color: var(--text);
    font: inherit;
    cursor: pointer;
  }
  .said {
    display: flex;
    gap: var(--space-2);
    align-items: flex-start;
    justify-content: space-between;
  }
  .dismiss {
    flex: none;
    height: auto;
    padding: 0 var(--space-1);
    border: 0;
    color: var(--muted);
  }
</style>
