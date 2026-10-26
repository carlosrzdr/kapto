<script lang="ts">
  import { DropdownMenu } from "bits-ui";

  import Icon from "./Icon.svelte";
  import { DEFAULT, isNamed, type PackageLayouts } from "./viewer/layout-list.svelte";

  interface Props {
    /** The package's layouts, the one shown and what the list last did. */
    layouts: PackageLayouts;
  }

  let { layouts }: Props = $props();

  let open = $state(false);
  /** What the box under the list asks, if anything. */
  let asking = $state<"rename" | "delete" | null>(null);
  let name = $state("");
  let picker = $state<HTMLInputElement | null>(null);

  const shown = $derived(layouts.shown);
  const named = $derived(shown !== null && isNamed(shown.id));
  const isDefault = $derived(shown?.id === DEFAULT);

  function ask(what: "rename" | "delete") {
    asking = what;
    name = shown?.name ?? "";
    layouts.dismiss();
  }

  async function answer(event: SubmitEvent) {
    event.preventDefault();
    if (layouts.clash) {
      await layouts.importAs(name);
    } else if (asking === "rename") {
      await layouts.rename(name);
    } else if (asking === "delete") {
      await layouts.remove();
    }
    if (!layouts.failed) {
      asking = null;
    }
  }

  function cancel() {
    asking = null;
    layouts.cancelImport();
  }

  async function download() {
    const file = await layouts.exportShown();
    if (!file) {
      return;
    }
    const url = URL.createObjectURL(new Blob([file.text], { type: "application/toml" }));
    const link = document.createElement("a");
    link.href = url;
    link.download = file.file;
    link.click();
    // After the browser has taken the file.
    setTimeout(() => URL.revokeObjectURL(url), 1000);
  }

  async function read(input: HTMLInputElement) {
    const file = input.files?.[0];
    input.value = "";
    if (!file) {
      return;
    }
    asking = null;
    await layouts.importFile(file.name, await file.text());
    if (layouts.clash) {
      name = layouts.clash.name;
    }
  }

  const prompt = $derived(layouts.clash ? "import" : asking);
</script>

<div class="layouts" role="group" aria-label="Layouts">
  <DropdownMenu.Root bind:open onOpenChange={(now) => now && void layouts.refresh()}>
    <DropdownMenu.Trigger class="layouts-opener" aria-label={`Layout: ${shown?.name ?? "loading"}`} disabled={!shown}>
      <span class="shown">{shown?.name ?? "Layout"}</span>
      <Icon name="chevron" />
    </DropdownMenu.Trigger>
    <DropdownMenu.Portal>
      <DropdownMenu.Content class="layouts-menu" aria-label="Layouts" align="end" sideOffset={4}>
        <DropdownMenu.RadioGroup value={layouts.shownId ?? ""} onValueChange={(id) => layouts.pick(id)} aria-label="Shown">
          {#each layouts.offers as offer (offer.id)}
            <DropdownMenu.RadioItem class="item" value={offer.id}>
              {#snippet children({ checked })}
                <span class="mark" aria-hidden="true">{checked ? "✓" : ""}</span>{offer.name}
              {/snippet}
            </DropdownMenu.RadioItem>
          {/each}
        </DropdownMenu.RadioGroup>
        <DropdownMenu.Separator class="rule" />
        <DropdownMenu.Item class="item" disabled={!named || isDefault} onSelect={() => ask("rename")}>Rename…</DropdownMenu.Item>
        <DropdownMenu.Item class="item" onSelect={() => void layouts.duplicate()}>Duplicate</DropdownMenu.Item>
        <DropdownMenu.Item class="item" disabled={!named} onSelect={() => ask("delete")}>Delete…</DropdownMenu.Item>
        <DropdownMenu.Item class="item" disabled={isDefault} onSelect={() => void layouts.saveAsDefault()}>Save as Default</DropdownMenu.Item>
        <DropdownMenu.Separator class="rule" />
        <DropdownMenu.Item class="item" onSelect={() => void download()}>Export</DropdownMenu.Item>
        <DropdownMenu.Item class="item" onSelect={() => picker?.click()}>Import…</DropdownMenu.Item>
      </DropdownMenu.Content>
    </DropdownMenu.Portal>
  </DropdownMenu.Root>
  <input
    class="picker"
    type="file"
    accept=".toml,application/toml"
    aria-label="Layout file to import"
    tabindex="-1"
    bind:this={picker}
    onchange={(event) => void read(event.currentTarget)}
  />

  {#if prompt || layouts.said}
    <div class="box">
      {#if prompt}
        <form aria-label={prompt === "import" ? "Import as" : prompt === "rename" ? "Rename the layout" : "Delete the layout"} onsubmit={answer}>
          {#if prompt === "delete"}
            <p>Delete {shown?.name} for every browser? Its file goes from the config folder.</p>
          {:else}
            <label>
              {prompt === "import" ? "Import as" : "Name"}
              <!-- svelte-ignore a11y_autofocus -->
              <input type="text" maxlength="64" required bind:value={name} autofocus />
            </label>
          {/if}
          <div class="actions">
            <button type="submit" class="go">{prompt === "import" ? "Import" : prompt === "rename" ? "Rename" : "Delete"}</button>
            <button type="button" onclick={cancel}>Cancel</button>
          </div>
        </form>
      {/if}
      {#if layouts.said}
        <div class="said">
          <p class:problem={layouts.failed} role={layouts.failed ? "alert" : "status"}>{layouts.said}</p>
          {#if !prompt}<button type="button" class="dismiss" aria-label="Dismiss" onclick={() => layouts.dismiss()}>×</button>{/if}
        </div>
      {/if}
    </div>
  {/if}
</div>

<style>
  .layouts {
    position: relative;
    display: flex;
  }
  .layouts :global(.layouts-opener) {
    display: inline-flex;
    gap: var(--space-2);
    align-items: center;
    max-width: 14rem;
    padding: 0 var(--space-2) 0 var(--space-3);
    border: 1px solid var(--control-line);
    border-radius: var(--radius);
    background: transparent;
    color: var(--text);
    font: inherit;
    font-size: var(--text-sm);
    cursor: pointer;
  }
  .layouts :global(.layouts-opener:hover:not(:disabled)),
  .layouts :global(.layouts-opener:focus-visible) {
    border-color: var(--text);
  }
  /* Its menu open: lit, as every button that opens a panel. */
  .layouts :global(.layouts-opener[data-state="open"]) {
    border-color: var(--text);
    background: var(--raised);
  }
  .layouts :global(.layouts-opener:disabled) {
    color: var(--muted);
    cursor: default;
  }
  /* The chevron points right elsewhere; here, down to the list. */
  .layouts :global(.layouts-opener .icon) {
    transform: rotate(90deg);
  }
  .shown {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  /* Opened by the menu's Import… only. */
  .picker {
    display: none;
  }
  /* The menu is drawn outside the page's flow, so its rules are global. */
  :global(.layouts-menu) {
    z-index: 40;
    display: flex;
    flex-direction: column;
    min-width: 12rem;
    max-height: min(24rem, var(--bits-dropdown-menu-content-available-height));
    overflow-y: auto;
    padding: var(--space-1);
    border: 1px solid var(--line);
    border-radius: var(--radius);
    background: var(--panel);
    color: var(--text);
    font-family: var(--font-ui);
    font-size: var(--text-sm);
    box-shadow: 0 8px 24px rgb(0 0 0 / 50%);
  }
  :global(.layouts-menu .item) {
    display: flex;
    flex: none;
    align-items: center;
    gap: var(--space-2);
    min-height: var(--control-height);
    padding: 0 var(--space-2);
    border-radius: var(--radius);
    cursor: pointer;
  }
  :global(.layouts-menu .item[data-highlighted]) {
    background: var(--raised);
    outline: none;
  }
  :global(.layouts-menu .item[aria-checked="true"]) {
    color: var(--accent);
  }
  :global(.layouts-menu .item[data-disabled]) {
    color: var(--muted);
    cursor: not-allowed;
  }
  :global(.layouts-menu .rule) {
    height: 1px;
    margin: var(--space-1) 0;
    background: var(--line);
  }
  .mark {
    width: 1ch;
    flex: none;
  }
  /* Under the list, over the panels while it asks or tells, never moving them. */
  .box {
    position: absolute;
    top: calc(100% + var(--space-1));
    right: 0;
    z-index: 3;
    display: flex;
    flex-direction: column;
    gap: var(--space-2);
    width: min(24rem, 90vw);
    padding: var(--space-2);
    border: 1px solid var(--line);
    border-radius: var(--radius);
    background: var(--panel);
    font-size: var(--text-sm);
    box-sizing: border-box;
  }
  form {
    display: flex;
    flex-direction: column;
    gap: var(--space-2);
  }
  label {
    display: flex;
    flex-direction: column;
    gap: var(--space-1);
    color: var(--muted);
  }
  input[type="text"] {
    height: var(--control-height);
    padding: 0 var(--space-2);
    border: 1px solid var(--control-line);
    border-radius: var(--radius);
    background: var(--background);
    color: var(--text);
    font: inherit;
  }
  p {
    margin: 0;
  }
  p[role="status"] {
    color: var(--muted);
  }
  .problem {
    color: var(--warning);
  }
  .actions {
    display: flex;
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
  .go {
    border-color: var(--accent);
  }
  .said {
    display: flex;
    gap: var(--space-2);
    align-items: flex-start;
    justify-content: space-between;
  }
  .dismiss {
    flex: none;
    height: auto;
    padding: 0 var(--space-1);
    border: 0;
    color: var(--muted);
  }
</style>
