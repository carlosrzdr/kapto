<script lang="ts">
  import { formatOf, MAX_FILE_BYTES, type Placed, placedMesh, placedShape, sampleCell, type Shape } from "./viewer/environment";

  interface Props {
    /** What is around the robot now. */
    placed: Placed[];
    /** Adds or changes one. */
    onput: (placed: Placed) => void;
    onremove: (id: string) => void;
    /** Says what went wrong, e.g. a file that can't be loaded. */
    problem?: string;
  }

  let { placed, onput, onremove, problem = "" }: Props = $props();

  let selected = $state<string | null>(null);
  let refused = $state("");
  let count = 0;

  const chosen = $derived(placed.find((each) => each.id === selected) ?? null);
  const said = $derived(refused || problem);

  const newId = () => `${Date.now().toString(36)}-${(count += 1)}`;

  function add(shape: Exclude<Shape, "mesh">) {
    const made = placedShape(shape, newId());
    // Numbered from the second of a name, so each can be told apart.
    const same = placed.filter((each) => each.name === made.name || each.name.startsWith(`${made.name} `)).length;
    if (same > 0) {
      made.name = `${made.name} ${same + 1}`;
    }
    onput(made);
    selected = made.id;
  }

  /** Adds a table, a cube and a bin, to start from. */
  function addCell() {
    for (const each of sampleCell(newId)) {
      onput(each);
    }
    selected = null;
  }

  async function load(input: HTMLInputElement) {
    const file = input.files?.[0];
    input.value = "";
    if (!file) {
      return;
    }
    const format = formatOf(file.name);
    if (!format) {
      refused = `${file.name} isn't a mesh this view loads: glTF (.glb, or .gltf holding everything), STL or OBJ.`;
      return;
    }
    if (file.size > MAX_FILE_BYTES) {
      refused = `${file.name} is larger than ${MAX_FILE_BYTES / 1024 / 1024} MiB; simplify it first.`;
      return;
    }
    refused = "";
    const made = placedMesh(file.name, format, await file.arrayBuffer(), newId());
    onput(made);
    selected = made.id;
  }

  /** Changes `axis` of `field` of the one chosen. */
  function set(field: "position" | "rotation" | "scale", axis: number, value: number) {
    if (!chosen || !Number.isFinite(value)) {
      return;
    }
    const next: [number, number, number] = [...chosen[field]];
    next[axis] = field === "scale" ? Math.max(value, 0.001) : value;
    onput({ ...chosen, [field]: next });
  }

  const FIELDS = [
    { field: "position", label: "Position (m)", step: 0.01, axes: ["x", "y", "z"] },
    { field: "rotation", label: "Rotation (°)", step: 5, axes: ["roll", "pitch", "yaw"] },
    { field: "scale", label: "Size", step: 0.01, axes: ["x", "y", "z"] },
  ] as const;
</script>

<section class="environment" aria-label="Environment">
  <p class="about">Shown around the robot in this browser only, live. Positions in the robot's frame, Z up.</p>
  <div class="add">
    <button type="button" onclick={() => add("box")}>Add box</button>
    <button type="button" onclick={() => add("cylinder")}>Add cylinder</button>
    <button type="button" onclick={() => add("sphere")}>Add sphere</button>
    <button type="button" title="A table, a red cube to pick and a bin, to start from" onclick={addCell}>Add a sample cell</button>
    <label class="file">
      Load a mesh…
      <input
        type="file"
        aria-label="Load a mesh"
        accept=".glb,.gltf,.stl,.obj"
        onchange={(event) => load(event.currentTarget)}
      />
    </label>
  </div>
  {#if said}
    <p class="problem" role="alert">{said}</p>
  {/if}
  {#if placed.length > 0}
    <ul aria-label="Around the robot">
      {#each placed as each (each.id)}
        <li>
          <button type="button" class:chosen={each.id === selected} onclick={() => (selected = each.id)}>{each.name}</button>
        </li>
      {/each}
    </ul>
  {/if}
  {#if chosen}
    <div class="edit" role="group" aria-label={`Placing ${chosen.name}`}>
      <label>
        Name
        <input type="text" value={chosen.name} onchange={(event) => onput({ ...chosen, name: event.currentTarget.value })} />
      </label>
      {#each FIELDS as { field, label, step, axes } (field)}
        <fieldset>
          <legend>{chosen.shape === "mesh" && field === "scale" ? "Scale" : label}</legend>
          {#each axes as axis, index (axis)}
            <label>
              {axis}
              <input
                type="number"
                aria-label={`${field} ${axis}`}
                {step}
                value={chosen[field][index]}
                onchange={(event) => set(field, index, event.currentTarget.valueAsNumber)}
              />
            </label>
          {/each}
        </fieldset>
      {/each}
      {#if chosen.format !== "glb" && chosen.format !== "gltf"}
        <label>
          Colour
          <input type="color" value={chosen.color} onchange={(event) => onput({ ...chosen, color: event.currentTarget.value })} />
        </label>
      {/if}
      <button
        type="button"
        class="remove"
        onclick={() => {
          onremove(chosen.id);
          selected = null;
        }}>Remove</button
      >
    </div>
  {/if}
</section>

<style>
  .environment {
    display: flex;
    flex-direction: column;
    gap: 0.5rem;
    padding: 0.6rem;
    font-size: var(--text-sm);
  }
  .about {
    margin: 0;
    color: var(--muted);
  }
  .add {
    display: flex;
    flex-wrap: wrap;
    gap: 0.4rem;
  }
  button,
  .file {
    padding: 0.2rem 0.6rem;
    border: 1px solid var(--control-line);
    border-radius: var(--radius);
    background: transparent;
    color: var(--text);
    font: inherit;
    cursor: pointer;
  }
  .file input {
    display: none;
  }
  .problem {
    margin: 0;
    color: var(--warning);
  }
  ul {
    display: flex;
    flex-wrap: wrap;
    gap: 0.3rem;
    margin: 0;
    padding: 0;
    list-style: none;
  }
  button.chosen {
    border-color: var(--accent);
  }
  .edit {
    display: flex;
    flex-direction: column;
    gap: 0.4rem;
  }
  fieldset {
    display: flex;
    gap: 0.4rem;
    margin: 0;
    padding: 0.2rem 0.4rem;
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
  input[type="number"],
  input[type="text"] {
    width: 4.5rem;
    padding: 0.1rem 0.25rem;
    border: 1px solid var(--control-line);
    border-radius: var(--radius);
    background: transparent;
    color: var(--text);
    font: inherit;
  }
  input[type="text"] {
    width: 12rem;
  }
  .remove {
    align-self: flex-start;
    border-color: var(--warning);
  }
</style>
