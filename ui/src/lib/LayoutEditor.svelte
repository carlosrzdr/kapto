<script lang="ts">
  import Choice from "./controls/Choice.svelte";
  import type { LayoutInput, PanelInput } from "./api-types";
  import { added, MAX_PANELS, MAX_PLOT_FIELDS, type Offered, panelCount } from "./viewer/layouts";

  interface Props {
    /** The layout being arranged. */
    layout: LayoutInput;
    /** What the package has that a panel may show. */
    offered: Offered;
    /** Streams the package records without decoding them: nothing to plot. */
    recordedOnly?: string[];
    /** Told each change, which is saved as it is made. */
    onchange: (layout: LayoutInput) => void;
  }

  let { layout, offered, recordedOnly = [], onchange }: Props = $props();

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

</script>

<section class="editor" aria-label="Arrange the view">
  <p class="about">
    Drag a panel by its grip, or an edge between two, on the view; or move a grip with the arrow keys. Saved as it is arranged, for
    every browser; <em>Package's</em> and <em>Made from roles</em> are first saved as a layout of their own.
  </p>
  <fieldset class="add">
    <legend>Add a panel</legend>
    <Choice
      label="Shows"
      value={kind}
      options={KINDS.map((each) => ({ value: each.kind, label: each.name }))}
      onchange={(value) => (kind = value)}
    />
    {#if kind === "camera"}
      <Choice
        label="Camera"
        value={stream}
        options={[{ value: "", label: "Choose one", disabled: true }, ...offered.cameras.map((camera) => ({ value: camera, label: camera }))]}
        onchange={(value) => (stream = value)}
      />
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
    <Choice
      label="In"
      value={String(column)}
      options={[
        ...layout.columns.map((_, place) => ({ value: String(place), label: `column ${place + 1}` })),
        { value: String(layout.columns.length), label: "a new column" },
      ]}
      onchange={(value) => (column = Number(value))}
    />
    <button type="button" disabled={!ready} onclick={add}>Add</button>
    {#if full}
      <p class="note">{MAX_PANELS} panels is as many as the page can draw.</p>
    {/if}
  </fieldset>
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
  button {
    min-width: var(--control-height);
    height: var(--control-height);
    padding: 0 var(--space-3);
    border: 1px solid var(--control-line);
    border-radius: var(--radius);
    background: transparent;
    color: var(--text);
    font: inherit;
    cursor: pointer;
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
</style>
