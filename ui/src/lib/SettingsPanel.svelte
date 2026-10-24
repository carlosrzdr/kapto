<script lang="ts">
  import Icon from "./Icon.svelte";
  import type { SettingsOverview } from "./api-types";
  import { breakable } from "./format";

  interface Props {
    settings: SettingsOverview;
  }

  let { settings }: Props = $props();

  const changed = $derived(settings.settings.filter((setting) => !setting.default).length);

  /** What to look for: every word must be in a setting's name, value or source. */
  let query = $state("");
  /** Whether to show only the settings that differ from their default. */
  let changedOnly = $state(false);

  const words = $derived(query.trim().toLowerCase().split(/\s+/).filter(Boolean));
  const shown = $derived(
    settings.settings.filter(
      (setting) =>
        (!changedOnly || !setting.default) &&
        words.every((word) => `${setting.key} ${setting.value} ${setting.source}`.toLowerCase().includes(word)),
    ),
  );
  const filtering = $derived(words.length > 0 || changedOnly);

  /**
   * The settings shown, by their section (the part of the name before its
   * first dot), each section where its first setting is, wherever the rest
   * of its settings come.
   */
  const sections = $derived(
    shown.reduce<{ name: string; settings: typeof shown }[]>((grouped, setting) => {
      const name = setting.key.split(".")[0] ?? setting.key;
      const section = grouped.find((each) => each.name === name);
      if (section) {
        section.settings.push(setting);
      } else {
        grouped.push({ name, settings: [setting] });
      }
      return grouped;
    }, []),
  );
</script>

<details>
  <summary>
    <Icon name="chevron" />
    {settings.settings.length} settings, {changed === 0 ? "all defaults" : `${changed} changed from the default`}
  </summary>
  <p class="file">
    {settings.file}{settings.file_found ? "" : " (not found, so not read)"}
  </p>
  <div class="filter">
    <input type="search" placeholder="Search settings" aria-label="Search settings" bind:value={query} />
    <label><input type="checkbox" bind:checked={changedOnly} /> Changed only</label>
  </div>
  {#if filtering}
    <p class="count" aria-live="polite">
      {#if shown.length === 0}
        No setting matches{words.length > 0 ? ` “${query.trim()}”` : ""}{changedOnly ? " among those changed" : ""}.
      {:else}
        {shown.length} of {settings.settings.length} settings
      {/if}
    </p>
  {/if}
  <table aria-label="Settings" hidden={shown.length === 0}>
    <thead>
      <tr>
        <th scope="col">Setting</th>
        <th scope="col">Value and where it came from</th>
      </tr>
    </thead>
    {#each sections as section (section.name)}
      <tbody>
        <tr class="section">
          <th scope="rowgroup" colspan="2">{section.name}</th>
        </tr>
        {#each section.settings as setting (setting.key)}
          <tr class:changed={!setting.default} title={setting.help}>
            <th scope="row"
              >{#each breakable(setting.key) as piece, index (index)}{piece}<wbr />{/each}{#if !setting.default}{" "}<span class="mark">changed</span>{/if}</th
            >
            <td>
              <span class="value"
                >{#each breakable(setting.value) as piece, index (index)}{piece}<wbr />{/each}{#if setting.unit}{" "}<span class="unit"
                    >{setting.unit}</span
                  >{/if}</span
              >
              <span class="source">{setting.source}</span>
            </td>
          </tr>
        {/each}
      </tbody>
    {/each}
  </table>
  <p class="note">Settings are read when Kapto starts; change them in the file, then start Kapto again.</p>
</details>

<style>
  summary {
    display: flex;
    align-items: center;
    gap: var(--space-2);
    list-style: none;
    cursor: pointer;
    color: var(--muted);
  }
  summary::-webkit-details-marker {
    display: none;
  }
  summary :global(.icon) {
    transition: transform 150ms ease-out;
  }
  details[open] > summary :global(.icon) {
    transform: rotate(90deg);
  }
  .file,
  .note,
  .count {
    margin: 0.5rem 0;
    color: var(--muted);
    font-size: var(--text-xs);
  }
  .file {
    font-family: var(--font-data);
    overflow-wrap: break-word;
  }
  .filter {
    display: flex;
    flex-wrap: wrap;
    gap: 0.5rem 1rem;
    align-items: center;
    margin: 0.5rem 0;
  }
  .filter input[type="search"] {
    flex: 1 1 12rem;
    min-width: 0;
    padding: 0.25rem 0.5rem;
    font: inherit;
    font-size: var(--text-sm);
    color: inherit;
    background: transparent;
    border: 1px solid var(--control-line);
    border-radius: 4px;
  }
  .filter label {
    color: var(--muted);
    font-size: var(--text-sm);
    white-space: nowrap;
  }
  table {
    width: 100%;
    border-collapse: collapse;
    font-size: var(--text-sm);
  }
  th,
  td {
    padding: 0.25rem 0.5rem;
    text-align: left;
    vertical-align: top;
    border-bottom: 1px solid var(--line);
    font-family: var(--font-data);
  }
  thead th {
    font-family: inherit;
    color: var(--muted);
    font-weight: 500;
    font-size: var(--text-xs);
  }
  th[scope="row"] {
    font-weight: 400;
  }
  tr:not(.changed) td {
    color: var(--muted);
  }
  .changed th,
  .changed td {
    color: var(--info);
  }
  .value {
    display: block;
    /* Not `anywhere`: that lets the table squeeze the column mid-word. */
    overflow-wrap: break-word;
  }
  .unit {
    color: var(--muted);
  }
  /* A section's heading: as the panel's own headings, a step above its rows. */
  .section th {
    padding-top: var(--space-3);
    font-family: var(--font-ui);
    font-weight: 500;
    color: var(--muted);
  }
  /* Changed: said in words as well as by the row's colour. */
  .mark {
    padding: 0 0.3rem;
    border: 1px solid currentColor;
    border-radius: var(--radius);
    font-family: var(--font-ui);
    font-size: var(--text-xs);
    white-space: nowrap;
  }
  .source {
    display: block;
    font-family: inherit;
    font-size: var(--text-xs);
    color: var(--muted);
  }
</style>
