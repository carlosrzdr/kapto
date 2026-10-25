<script lang="ts" module>
  /** One thing to choose: what it is, how it reads, and why it can't be chosen if it can't. */
  export interface Option {
    value: string;
    label: string;
    /** A second line in the list, e.g. who the profile's operator is. */
    detail?: string;
    disabled?: boolean;
  }
</script>

<script lang="ts">
  import { Select } from "bits-ui";

  import Icon from "../Icon.svelte";

  interface Props {
    /** What is chosen, shown beside it and given to screen readers. */
    label: string;
    /** Whether `label` is shown, or only said, where what is chosen says it already. */
    showLabel?: boolean;
    /** Said to screen readers instead of `label`, when that alone is unclear. */
    name?: string;
    value: string;
    options: Option[];
    disabled?: boolean;
    /** The list's widest, so a long name doesn't stretch the page. */
    width?: "compact" | "narrow" | "wide";
    onchange: (value: string) => void;
    /** The list was opened: worth reading what it holds again. */
    onopen?: () => void;
  }

  let { label, showLabel = true, name, value, options, disabled = false, width = "narrow", onchange, onopen }: Props = $props();

  const chosen = $derived(options.find((option) => option.value === value));
</script>

<div class="choice">
  {#if showLabel}<span class="label" aria-hidden="true">{label}</span>{/if}
  <Select.Root
    type="single"
    {value}
    {disabled}
    items={options.map(({ value, label, disabled }) => ({ value, label, disabled }))}
    onValueChange={onchange}
    onOpenChange={(open) => open && onopen?.()}
  >
    <Select.Trigger class={["trigger", width]} aria-label={name ?? label}>
      <span class="chosen">{chosen?.label ?? value}</span>
      <Icon name="chevron" />
    </Select.Trigger>
    <Select.Portal>
      <Select.Content class="list" sideOffset={4} align="start">
        <Select.Viewport>
          {#each options as option (option.value)}
            <!-- Bits UI marks a disabled option for the eye only: said to screen readers too. -->
            <Select.Item
              class="option"
              value={option.value}
              label={option.label}
              disabled={option.disabled}
              aria-disabled={option.disabled ? "true" : undefined}
            >
              {#snippet children({ selected })}
                <span class="mark" aria-hidden="true">{selected ? "✓" : ""}</span>
                <span class="text">
                  <span>{option.label}</span>
                  {#if option.detail}<span class="detail">{option.detail}</span>{/if}
                </span>
              {/snippet}
            </Select.Item>
          {/each}
        </Select.Viewport>
      </Select.Content>
    </Select.Portal>
  </Select.Root>
</div>

<style>
  .choice {
    display: flex;
    gap: var(--space-2);
    align-items: center;
  }
  .label {
    color: var(--muted);
    font-size: var(--text-sm);
  }
  .choice :global(.trigger) {
    display: inline-flex;
    gap: var(--space-2);
    align-items: center;
    justify-content: space-between;
    min-width: 7rem;
    height: var(--control-height);
    padding: 0 var(--space-2);
    border: 1px solid var(--control-line);
    border-radius: var(--radius);
    background: var(--panel);
    color: var(--text);
    font: inherit;
    font-size: var(--text-sm);
    font-weight: 400;
    cursor: pointer;
  }
  .choice :global(.trigger.compact) {
    min-width: 4.5rem;
  }
  .choice :global(.trigger.narrow) {
    max-width: 12rem;
  }
  .choice :global(.trigger.wide) {
    max-width: 18rem;
  }
  .choice :global(.trigger:hover:not(:disabled)) {
    border-color: var(--text);
  }
  .choice :global(.trigger:focus-visible) {
    outline: 2px solid var(--ring);
    outline-offset: 2px;
  }
  /* Can't be changed for now: no edge or chevron offering to. */
  .choice :global(.trigger:disabled) {
    border-color: var(--line);
    color: var(--muted);
    cursor: not-allowed;
  }
  .choice :global(.trigger:disabled .icon) {
    visibility: hidden;
  }
  /* The chevron points right elsewhere; here, down to the list. */
  .choice :global(.trigger .icon) {
    transform: rotate(90deg);
  }
  .chosen {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  /* The list is drawn outside the page's flow, so its rules are global. */
  :global(.list) {
    z-index: 50;
    min-width: var(--bits-select-anchor-width);
    max-height: min(20rem, var(--bits-select-content-available-height));
    overflow-y: auto;
    padding: var(--space-1);
    border: 1px solid var(--line);
    border-radius: var(--radius);
    background: var(--raised);
    color: var(--text);
    font-family: var(--font-ui);
    font-size: var(--text-sm);
    box-shadow: 0 8px 24px rgb(0 0 0 / 50%);
  }
  :global(.option) {
    display: flex;
    gap: var(--space-2);
    align-items: flex-start;
    padding: var(--space-1) var(--space-2);
    border-radius: var(--radius);
    cursor: pointer;
  }
  :global(.option[data-highlighted]) {
    background: var(--panel);
    outline: 1px solid var(--control-line);
  }
  :global(.option[data-selected]) {
    color: var(--accent);
  }
  :global(.option[data-disabled]) {
    color: var(--muted);
    cursor: not-allowed;
  }
  .mark {
    width: 1ch;
    flex: none;
  }
  .text {
    display: flex;
    flex-direction: column;
  }
  .detail {
    color: var(--muted);
    font-size: var(--text-xs);
  }
</style>
