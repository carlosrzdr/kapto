<script lang="ts">
  import Icon from "./Icon.svelte";
  /** One tab: its name, and whether it holds something that needs attention. */
  export interface Tab {
    id: string;
    label: string;
    alert?: boolean;
  }

  interface Props {
    tabs: Tab[];
    selected: string;
    onselect: (id: string) => void;
  }

  let { tabs, selected, onselect }: Props = $props();

  /** Arrow keys move between tabs, as in any tab list. */
  function keydown(event: KeyboardEvent, index: number) {
    const step = event.key === "ArrowRight" ? 1 : event.key === "ArrowLeft" ? -1 : 0;
    if (step === 0) {
      return;
    }
    const next = tabs[(index + step + tabs.length) % tabs.length];
    if (next) {
      onselect(next.id);
      document.getElementById(`tab-${next.id}`)?.focus();
    }
  }
</script>

<div role="tablist">
  {#each tabs as tab, index (tab.id)}
    <button
      type="button"
      role="tab"
      id="tab-{tab.id}"
      aria-selected={tab.id === selected}
      aria-controls="panel-{tab.id}"
      tabindex={tab.id === selected ? 0 : -1}
      class:alert={tab.alert}
      aria-label={tab.alert ? `${tab.label} needs attention` : undefined}
      onclick={() => onselect(tab.id)}
      onkeydown={(event) => keydown(event, index)}
    >
      {tab.label}{#if tab.alert}<span class="attention"><Icon name="attention" /></span>{/if}
    </button>
  {/each}
</div>

<style>
  [role="tablist"] {
    display: flex;
    gap: 0.25rem;
    border-bottom: 1px solid var(--line);
    margin-bottom: 0.75rem;
  }
  button {
    padding: 0.4rem 0.6rem;
    border: none;
    border-bottom: 2px solid transparent;
    background: none;
    color: var(--muted);
    font: inherit;
    cursor: pointer;
  }
  button[aria-selected="true"] {
    color: var(--text);
    border-bottom-color: var(--accent);
  }
  .attention {
    margin-left: var(--space-1);
    color: var(--warning);
    font-size: 0.7em;
  }
</style>
