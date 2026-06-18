<script lang="ts">
  import type { LayoutInput, PanelInput } from "./api-types";
  import {
    added,
    heightened,
    MAX_PANELS,
    MAX_PLOT_FIELDS,
    type Move,
    moved,
    type Offered,
    panelCount,
    removed,
    widened,
  } from "./viewer/layouts";

  interface Props {
    /** The layout being arranged. */
    layout: LayoutInput;
    /** What the package has that a panel may show. */
    offered: Offered;
    /** Streams the package records without decoding them: nothing to plot. */
    recordedOnly?: string[];
    /** Told each change. */
    onchange: (layout: LayoutInput) => void;
    /** Saves the layout as the package's default, for every browser. */
    onsave: () => void;
    /** Forgets this browser's layout and the package's default. */
    onreset: () => void;
    /** What the last save or reset did, or why it failed. */
    said?: string;
    /** Whether that failed. */
    failed?: boolean;
  }

  let { layout, offered, recordedOnly = [], onchange, onsave, onreset, said = "", failed = false }: Props = $props();

  const KINDS = [
    { kind: "robot", name: "Robot" },
    { kind: "camera", name: "Camera" },
    { kind: "plot", name: "Plot" },
    { kind: "health", name: "Streams' health" },
    { kind: "events", name: "Marks" },
  ];

  let kind = $state("plot");
  let stream = $state("");
  let picked = $state<string[]>([]);
  let column = $state(0);

  const full = $derived(panelCount(layout) >= MAX_PANELS);
  const choices = $derived([...offered.roles.map((role) => `role:${role}`), ...offered.fields]);
  const ready = $derived(
    !full && (kind === "camera" ? offered.cameras.includes(stream) : kind === "plot" ? picked.length > 0 : true),
  );

  /** What a panel shows, in words. */
  function named(panel: PanelInput): string {
    switch (panel.kind) {
      case "camera":
        return `Camera ${panel.stream ?? ""}`;
      case "plot":
        return `Plot of ${(panel.fields ?? []).join(", ")}`;
      default:
        return KINDS.find((each) => each.kind === panel.kind)?.name ?? panel.kind;
    }
  }

  function add() {
    const panel: PanelInput = { kind, height: 1, fields: kind === "plot" ? picked : [] };
    if (kind === "camera") {
      panel.stream = stream;
    }
    onchange(added(layout, column, panel));
    picked = [];
  }

  function pick(choice: string, on: boolean) {
    picked = on ? [...picked, choice].slice(0, MAX_PLOT_FIELDS) : picked.filter((each) => each !== choice);
  }

  const MOVES: { move: Move; sign: string; words: string }[] = [
    { move: "up", sign: "↑", words: "up" },
    { move: "down", sign: "↓", words: "down" },
    { move: "left", sign: "←", words: "to the column on the left" },
    { move: "right", sign: "→", words: "to the column on the right" },
  ];
</script>

<section class="editor" aria-label="Arrange the view">
  <p class="about">
    Arranged in this browser, for this robot package. <em>Save as default</em> arranges every browser's view this way.
  </p>
  <ol class="columns">
    {#each layout.columns as each, place (place)}
      <li>
        <div class="column-head">
          <span>Column {place + 1}</span>
          <span class="share">
            width {each.width}
            <button type="button" aria-label={`Column ${place + 1} narrower`} onclick={() => onchange(widened(layout, place, -1))}
              >−</button
            >
            <button type="button" aria-label={`Column ${place + 1} wider`} onclick={() => onchange(widened(layout, place, 1))}>+</button>
          </span>
        </div>
        <ul>
          {#each each.panels as panel, at (at)}
            <li>
              <span class="name" title={named(panel)}>{named(panel)}</span>
              <span class="controls">
                {#each MOVES as { move, sign, words } (move)}
                  <button
                    type="button"
                    aria-label={`Move ${named(panel)} ${words}`}
                    title={`Move ${words}`}
                    onclick={() => onchange(moved(layout, place, at, move))}>{sign}</button
                  >
                {/each}
                <span class="share">
                  height {panel.height}
                  <button type="button" aria-label={`${named(panel)} shorter`} onclick={() => onchange(heightened(layout, place, at, -1))}
                    >−</button
                  >
                  <button type="button" aria-label={`${named(panel)} taller`} onclick={() => onchange(heightened(layout, place, at, 1))}
                    >+</button
                  >
                </span>
                <button type="button" class="remove" aria-label={`Remove ${named(panel)}`} onclick={() => onchange(removed(layout, place, at))}
                  >Remove</button
                >
              </span>
            </li>
          {/each}
        </ul>
      </li>
    {/each}
  </ol>

  <fieldset class="add">
    <legend>Add a panel</legend>
    <label>
      Shows
      <select bind:value={kind}>
        {#each KINDS as each (each.kind)}
          <option value={each.kind}>{each.name}</option>
        {/each}
      </select>
    </label>
    {#if kind === "camera"}
      <label>
        Camera
        <select bind:value={stream}>
          <option value="" disabled>Choose one</option>
          {#each offered.cameras as camera (camera)}
            <option value={camera}>{camera}</option>
          {/each}
        </select>
      </label>
      {#if offered.cameras.length === 0}
        <p class="note">The package has no camera streams.</p>
      {/if}
    {:else if kind === "plot"}
      <fieldset class="fields">
        <legend>Fields and roles, up to {MAX_PLOT_FIELDS}</legend>
        {#each choices as choice (choice)}
          <label class="choice">
            <input
              type="checkbox"
              checked={picked.includes(choice)}
              disabled={!picked.includes(choice) && picked.length >= MAX_PLOT_FIELDS}
              onchange={(event) => pick(choice, event.currentTarget.checked)}
            />
            {choice}
          </label>
        {/each}
        {#each recordedOnly as name (name)}
          <p class="note">{name}: recorded only, nothing decoded to plot</p>
        {/each}
      </fieldset>
    {/if}
    <label>
      In
      <select bind:value={column}>
        {#each layout.columns as _, place (place)}
          <option value={place}>column {place + 1}</option>
        {/each}
        <option value={layout.columns.length}>a new column</option>
      </select>
    </label>
    <button type="button" disabled={!ready} onclick={add}>Add</button>
    {#if full}
      <p class="note">{MAX_PANELS} panels is as many as the page can draw.</p>
    {/if}
  </fieldset>

  <div class="actions">
    <button type="button" class="save" onclick={onsave}>Save as default</button>
    <button type="button" title="Forget this browser's arrangement and the saved default" onclick={onreset}
      >Reset to the package's</button
    >
  </div>
  {#if said}
    <p class:problem={failed} role={failed ? "alert" : "status"}>{said}</p>
  {/if}
</section>

<style>
  .editor {
    display: flex;
    flex-direction: column;
    gap: 0.5rem;
    padding: 0.6rem;
    font-size: var(--text-sm);
  }
  .about,
  .note {
    margin: 0;
    color: var(--muted);
  }
  .note {
    font-size: var(--text-xs);
  }
  .columns,
  ul {
    display: flex;
    flex-direction: column;
    gap: 0.3rem;
    margin: 0;
    padding: 0;
    list-style: none;
  }
  .columns > li {
    padding: 0.3rem 0.4rem;
    border: 1px solid var(--line);
    border-radius: var(--radius);
  }
  .column-head,
  ul li {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    justify-content: space-between;
    gap: 0.3rem;
  }
  .column-head {
    color: var(--muted);
  }
  /* A plot of many fields names them all in its tooltip. */
  .name {
    display: -webkit-box;
    -webkit-box-orient: vertical;
    -webkit-line-clamp: 2;
    line-clamp: 2;
    overflow: hidden;
    overflow-wrap: anywhere;
  }
  .controls,
  .share,
  .actions {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 0.25rem;
  }
  .share {
    color: var(--muted);
    font-size: var(--text-xs);
  }
  button,
  select {
    padding: 0.1rem 0.45rem;
    border: 1px solid var(--control-line);
    border-radius: var(--radius);
    background: transparent;
    color: var(--text);
    font: inherit;
    cursor: pointer;
  }
  select {
    background: var(--panel);
  }
  .remove {
    border-color: var(--warning);
  }
  .save {
    border-color: var(--accent);
  }
  fieldset {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 0.4rem;
    margin: 0;
    padding: 0.3rem 0.5rem;
    border: 1px solid var(--line);
    border-radius: var(--radius);
  }
  legend {
    color: var(--muted);
  }
  label {
    display: flex;
    align-items: center;
    gap: 0.3rem;
    color: var(--muted);
  }
  .fields {
    flex-direction: column;
    flex-wrap: nowrap;
    align-items: flex-start;
    width: 100%;
    max-height: 12rem;
    overflow-y: auto;
  }
  .choice {
    color: var(--text);
    font-family: var(--font-data);
    font-size: var(--text-xs);
  }
  .problem {
    margin: 0;
    color: var(--warning);
  }
  p[role="status"] {
    margin: 0;
    color: var(--muted);
  }
</style>
