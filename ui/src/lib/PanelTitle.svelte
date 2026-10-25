<script lang="ts">
  import { MAX_PANEL_TITLE } from "./viewer/layouts";

  interface Props {
    /** The panel's own title, if it has one. */
    title: string | null;
    /** The title made from what the panel shows, shown while nothing is written. */
    made: string;
    /** What the field is, for a screen reader. */
    label: string;
    /** Told what was written once Enter is pressed (`byKey`) or the field is left: blank, the panel goes back to its made title. */
    onsave: (written: string, byKey: boolean) => void;
    /** Told when Escape leaves the title as it was. */
    oncancel: () => void;
  }

  let { title, made, label, onsave, oncancel }: Props = $props();
  let written = $state("");
  let done = false;

  /** The field starts from the title it had and takes the focus with it selected, to be written over or added to. */
  function focusAll(field: HTMLInputElement) {
    const had = title ?? "";
    written = had;
    field.value = had;
    field.focus();
    field.select();
  }

  /** Saves once, or cancels once: the field goes as soon as either is told. */
  function finish(save: boolean, byKey: boolean) {
    if (done) {
      return;
    }
    done = true;
    if (save) {
      onsave(written, byKey);
    } else {
      oncancel();
    }
  }

  function key(event: KeyboardEvent) {
    if (event.key !== "Enter" && event.key !== "Escape") {
      return;
    }
    event.preventDefault();
    // Escape is the field's here, not the page's.
    event.stopPropagation();
    finish(event.key === "Enter", true);
  }
</script>

<input
  class="title-field"
  type="text"
  maxlength={MAX_PANEL_TITLE}
  aria-label={label}
  title="Enter to save, Escape to leave it as it was; left empty, the panel is named from what it shows"
  placeholder={made}
  bind:value={written}
  {@attach focusAll}
  onkeydown={key}
  onblur={() => finish(true, false)}
/>

<style>
  /* Written in place of the grip's name, as wide as the bar lets it be. */
  .title-field {
    flex: 1;
    min-width: 0;
    margin: 0.25rem;
    padding: 0.1rem 0.45rem;
    border: 1px solid var(--accent);
    border-radius: var(--radius);
    background: var(--background);
    color: var(--text);
    font: inherit;
    font-size: var(--text-xs);
  }
</style>
