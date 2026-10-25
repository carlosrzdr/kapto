<script lang="ts">
  import { Tooltip } from "bits-ui";
  import type { Snippet } from "svelte";

  interface Props {
    /** What it says: shown on hover and on focus, a moment after. */
    text: string;
    /**
     * What it is about, given the trigger's attributes to spread on its own
     * element, so that element is what is hovered and focused.
     */
    children: Snippet<[Record<string, unknown>]>;
    /** On which side of what it is about it shows. */
    side?: "top" | "bottom" | "left" | "right";
  }

  let { text, children, side = "bottom" }: Props = $props();
</script>

<!-- A provider of its own, so it works wherever it is placed, a test's component included. -->
<Tooltip.Provider delayDuration={300}>
  <Tooltip.Root>
    <Tooltip.Trigger>
      {#snippet child({ props })}
        {@render children(props)}
      {/snippet}
    </Tooltip.Trigger>
    <Tooltip.Portal>
      <!-- Bits UI leaves the role to us. -->
      <Tooltip.Content class="hint" role="tooltip" {side} sideOffset={6} collisionPadding={8}>{text}</Tooltip.Content>
    </Tooltip.Portal>
  </Tooltip.Root>
</Tooltip.Provider>

<style>
  /* Drawn over the page, so its rules are global. */
  :global(.hint) {
    z-index: 70;
    max-width: 22rem;
    padding: var(--space-1) var(--space-2);
    border: 1px solid var(--line);
    border-radius: var(--radius);
    background: var(--raised);
    color: var(--text);
    font-family: var(--font-ui);
    font-size: var(--text-xs);
    line-height: 1.4;
    box-shadow: 0 4px 12px rgb(0 0 0 / 50%);
  }
</style>
