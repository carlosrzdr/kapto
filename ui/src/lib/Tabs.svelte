<script lang="ts">
  import Icon, { type IconName } from "./Icon.svelte";
  /** One tab: its name, and how badly what it holds needs attention, if it does. */
  export interface Tab {
    id: string;
    label: string;
    /** What stands for it on the strip the folded panel leaves. */
    icon: IconName;
    /** In the colour the same problem has everywhere else: the health chip's. */
    alert?: "warning" | "danger" | null;
  }

  interface Props {
    tabs: Tab[];
    selected: string;
    onselect: (id: string) => void;
    /** Whether the panel is folded to a strip of the tabs' icons. */
    folded?: boolean;
    /** Folds the panel or opens it again; without it, the panel can't fold. */
    onfold?: (folded: boolean) => void;
  }

  let { tabs, selected, onselect, folded = false, onfold }: Props = $props();

  /** Arrow keys move between tabs, Home and End to the first and last, as in any tab list. */
  function keydown(event: KeyboardEvent, index: number) {
    const to = {
      ArrowRight: (index + 1) % tabs.length,
      ArrowLeft: (index - 1 + tabs.length) % tabs.length,
      Home: 0,
      End: tabs.length - 1,
    }[event.key];
    if (to === undefined) {
      return;
    }
    event.preventDefault();
    const next = tabs[to];
    if (next) {
      onselect(next.id);
      document.getElementById(`tab-${next.id}`)?.focus();
    }
  }
</script>

<div class="tabs" class:folded>
  {#if onfold}
    <!-- One button in both states, so the focus stays on it as it folds. -->
    <button
      type="button"
      class="fold"
      aria-expanded={!folded}
      aria-label={folded ? "Open the side panel" : "Fold the side panel"}
      title={folded ? "Open the side panel (P)" : "Fold the side panel, for a wider view (P)"}
      onclick={() => onfold(!folded)}><Icon name="chevron" /></button
    >
  {/if}
  {#if folded}
    <!-- Not a tab list: no panel is shown; each opens the panel on its tab. -->
    {#each tabs as tab (tab.id)}
      <button
        type="button"
        class="strip"
        aria-label={tab.alert ? `${tab.label} needs attention` : tab.label}
        title={tab.label}
        onclick={() => onselect(tab.id)}
      >
        <Icon name={tab.icon} />{#if tab.alert}<span class="attention {tab.alert}"><Icon name="attention" /></span>{/if}
      </button>
    {/each}
  {:else}
    <div role="tablist">
      {#each tabs as tab, index (tab.id)}
        <button
          type="button"
          role="tab"
          id="tab-{tab.id}"
          aria-selected={tab.id === selected}
          aria-controls="panel-{tab.id}"
          tabindex={tab.id === selected ? 0 : -1}
          class:alert={!!tab.alert}
          aria-label={tab.alert ? `${tab.label} needs attention` : undefined}
          onclick={() => onselect(tab.id)}
          onkeydown={(event) => keydown(event, index)}
        >
          {tab.label}{#if tab.alert}<span class="attention {tab.alert}"><Icon name="attention" /></span>{/if}
        </button>
      {/each}
    </div>
  {/if}
</div>

<style>
  .tabs {
    display: flex;
    border-bottom: 1px solid var(--line);
    margin-bottom: 0.75rem;
  }
  [role="tablist"] {
    display: flex;
    gap: 0.25rem;
  }
  /* The strip: the fold button over one icon per tab, as narrow as they are. */
  .tabs.folded {
    flex-direction: column;
    align-items: center;
    gap: var(--space-1);
    border-bottom: none;
    margin-bottom: 0;
  }
  .fold,
  .strip {
    position: relative;
    display: inline-flex;
    align-items: center;
    justify-content: center;
    width: var(--control-height);
    height: var(--control-height);
    padding: 0;
    border-radius: var(--radius);
  }
  .strip {
    font-size: var(--text-lg);
  }
  .fold:hover,
  .strip:hover {
    color: var(--text);
  }
  .fold:focus-visible,
  .strip:focus-visible {
    outline: 2px solid var(--ring);
    outline-offset: -2px;
  }
  /* Pointing where the panel goes: away to fold it, back to open it. */
  .folded .fold :global(.icon) {
    transform: rotate(180deg);
  }
  .strip .attention {
    position: absolute;
    top: 0.2rem;
    right: 0.2rem;
    margin: 0;
    line-height: 0;
  }
  button {
    padding: 0.4rem var(--inset);
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
    font-size: 0.7em;
  }
  .attention.warning {
    color: var(--warning);
  }
  .attention.danger {
    color: var(--danger);
  }
</style>
