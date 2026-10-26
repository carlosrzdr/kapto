<script lang="ts" module>
  /** Where the editor was folded and on which side it was left, in this browser only. */
  export const ARRANGING_KEY = "kapto.arranging";

  /** How the operator last left the editor. */
  export interface ArrangingPlace {
    folded: boolean;
    side: "left" | "right";
  }

  /** How the editor was last left, or open on the right if never or unreadable. */
  export function rememberedPlace(storage?: Storage): ArrangingPlace {
    try {
      const kept = JSON.parse((storage ?? localStorage).getItem(ARRANGING_KEY) ?? "{}") as Partial<ArrangingPlace>;
      return { folded: kept.folded === true, side: kept.side === "left" ? "left" : "right" };
    } catch {
      return { folded: false, side: "right" };
    }
  }
</script>

<script lang="ts">
  import type { Snippet } from "svelte";

  import Icon from "./Icon.svelte";

  interface Props {
    /** While an edge is dragged or a panel carried: see-through and never in the pointer's way. */
    faded: boolean;
    /** Where it is remembered; the page's own storage when left out. */
    storage?: Storage;
    children: Snippet;
  }

  let { faded, storage, children }: Props = $props();

  // svelte-ignore state_referenced_locally
  let place = $state(rememberedPlace(storage));

  function keep(next: ArrangingPlace) {
    place = next;
    try {
      (storage ?? localStorage).setItem(ARRANGING_KEY, JSON.stringify(next));
    } catch {
      // Not remembered, then; it stays as it is for this page.
    }
  }
</script>

<!-- Over the view rather than beside it, so the columns arranged are as wide
     as they will be shown; folded or moved aside when it covers a panel. -->
<div class="arranging" id="arranging" class:faded class:folded={place.folded} class:left={place.side === "left"}>
  <div class="bar">
    <h2>Arranging</h2>
    <button
      type="button"
      class="side"
      onclick={() => keep({ ...place, side: place.side === "right" ? "left" : "right" })}
    >
      {place.side === "right" ? "Move to the left" : "Move to the right"}
    </button>
    <button
      type="button"
      class="fold"
      aria-expanded={!place.folded}
      aria-controls="arranging-body"
      aria-label={place.folded ? "Unfold the editor" : "Fold the editor"}
      title={place.folded ? "Unfold the editor" : "Fold the editor, to see the panels under it"}
      onclick={() => keep({ ...place, folded: !place.folded })}><Icon name="chevron" /></button
    >
  </div>
  <div class="body" id="arranging-body" hidden={place.folded}>
    {@render children()}
  </div>
</div>

<style>
  /* Right under Arrange, over the panels: above them, under the guides
     drawn while dragging. */
  .arranging {
    position: absolute;
    top: calc(var(--control-height) + var(--space-1));
    right: 0;
    z-index: 20;
    display: flex;
    flex-direction: column;
    width: min(30rem, 100%);
    max-height: calc(100% - 6rem);
    border: 1px solid var(--line);
    border-top-color: var(--accent);
    border-radius: var(--radius);
    background: var(--panel);
    box-shadow: var(--shadow-floating);
    /* Its edges inside its width, so it ends where its button and the view do. */
    box-sizing: border-box;
  }
  .arranging.left {
    right: auto;
    left: 0;
  }
  .folded {
    width: auto;
  }
  .faded {
    opacity: 0.25;
    pointer-events: none;
  }
  .bar {
    display: flex;
    flex: none;
    align-items: center;
    gap: var(--space-2);
    padding: var(--space-1) var(--space-1) var(--space-1) var(--space-3);
  }
  .bar:not(:last-child) {
    border-bottom: 1px solid var(--line);
  }
  h2 {
    flex: 1;
    margin: 0;
    font-size: var(--text-sm);
    font-weight: 500;
  }
  button {
    min-width: var(--control-height);
    height: var(--control-height);
    padding: 0 var(--space-2);
    border: 1px solid var(--control-line);
    border-radius: var(--radius);
    background: transparent;
    color: var(--text);
    font: inherit;
    font-size: var(--text-xs);
    cursor: pointer;
  }
  .fold {
    display: grid;
    place-items: center;
    padding: 0;
  }
  /* Down while open, up into the bar to fold it. */
  .fold :global(.icon) {
    transform: rotate(-90deg);
  }
  .folded .fold :global(.icon) {
    transform: rotate(90deg);
  }
  .body {
    min-height: 0;
    overflow-y: auto;
  }
  .body[hidden] {
    display: none;
  }
  /* No room to float, as on a tablet: between its button and the panels,
     taking room, so it never covers them; no side to move it to. */
  @media (max-width: 70rem) {
    .arranging,
    .arranging.left {
      position: static;
      flex: none;
      width: 100%;
      max-height: 40%;
      margin-bottom: var(--space-2);
      box-shadow: none;
    }
    .side {
      display: none;
    }
  }
</style>
