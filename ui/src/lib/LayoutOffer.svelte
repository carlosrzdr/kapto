<script lang="ts">
  interface Props {
    /** What is offered, in a sentence. */
    said: string;
    /** The choice that shows the other layout, e.g. "Use the new one". */
    use: string;
    /** The choice that keeps a layout under a name, e.g. "Save it as…". */
    save: string;
    /** The name offered for it, not yet taken. */
    suggested: string;
    onuse: () => void;
    /** Saves under `name`; answers why it couldn't, if it couldn't. */
    onsave: (name: string) => Promise<string | null>;
    onkeep: () => void;
  }

  let { said, use, save, suggested, onuse, onsave, onkeep }: Props = $props();

  /** The name asked for while saving; none while not asking. */
  let name = $state<string | null>(null);
  let problem = $state("");
  let saving = $state(false);

  async function answer(event: SubmitEvent) {
    event.preventDefault();
    if (name === null || saving) {
      return;
    }
    saving = true;
    problem = (await onsave(name)) ?? "";
    saving = false;
    if (!problem) {
      name = null;
    }
  }
</script>

<!-- On a line of its own above the panels, in the page's flow: it moves
     them down rather than covering what they show or the view's controls,
     and goes once one of its choices is made. -->
<section class="offer" aria-label="Another layout">
  {#if name === null}
    <p>{said}</p>
    <div class="actions">
      <button type="button" class="go" onclick={onuse}>{use}</button>
      <button type="button" onclick={() => ((name = suggested), (problem = ""))}>{save}</button>
      <button type="button" onclick={onkeep}>Keep this one</button>
    </div>
  {:else}
    <form aria-label={save.replace("…", "")} onsubmit={answer}>
      <label>
        Name
        <!-- svelte-ignore a11y_autofocus -->
        <input type="text" maxlength="64" required bind:value={name} autofocus />
      </label>
      <div class="actions">
        <button type="submit" class="go" disabled={saving}>Save</button>
        <button type="button" onclick={() => ((name = null), (problem = ""))}>Cancel</button>
      </div>
    </form>
    {#if problem}<p class="problem" role="alert">{problem}</p>{/if}
  {/if}
</section>

<style>
  .offer {
    display: flex;
    flex: none;
    flex-wrap: wrap;
    gap: var(--space-2) var(--space-3);
    align-items: center;
    justify-content: space-between;
    margin-bottom: var(--space-1);
    padding: var(--space-2) var(--space-3);
    border: 1px solid var(--line);
    border-left: 3px solid var(--accent);
    border-radius: var(--radius);
    background: var(--panel);
    font-size: var(--text-sm);
  }
  p {
    margin: 0;
  }
  form {
    display: flex;
    flex-wrap: wrap;
    gap: var(--space-2);
    align-items: center;
  }
  label {
    display: flex;
    gap: var(--space-2);
    align-items: center;
    color: var(--muted);
  }
  input[type="text"] {
    width: min(16rem, 50vw);
    height: var(--control-height);
    padding: 0 var(--space-2);
    border: 1px solid var(--control-line);
    border-radius: var(--radius);
    background: var(--background);
    color: var(--text);
    font: inherit;
  }
  .actions {
    display: flex;
    flex-wrap: wrap;
    gap: var(--space-1);
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
  button:hover:not(:disabled),
  button:focus-visible {
    border-color: var(--text);
  }
  .go {
    border-color: var(--accent);
  }
  .problem {
    flex-basis: 100%;
    color: var(--warning);
  }
</style>
