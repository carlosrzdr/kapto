<script lang="ts">
  import type { SessionChoice } from "./api";
  import type { ProfilesOverview, TasksOverview } from "./api-types";

  interface Props {
    profiles: ProfilesOverview | null;
    tasks: TasksOverview | null;
    choice: SessionChoice;
    /** While recording: the choice is for the next episode. */
    disabled: boolean;
    onchange: (choice: SessionChoice) => void;
    /** A list was opened: worth reading the saved ones again. */
    onopen: () => void;
  }

  let { profiles, tasks, choice, disabled, onchange, onopen }: Props = $props();
</script>

<div class="session">
  <label>
    Profile
    <select
      value={choice.profile ?? ""}
      {disabled}
      onfocus={onopen}
      onchange={(event) => onchange({ ...choice, profile: event.currentTarget.value || null })}
    >
      <option value="">None</option>
      {#each profiles?.profiles ?? [] as profile (profile.id)}
        <option value={profile.id} disabled={!profile.available}>
          {profile.id}{profile.operator ? ` · ${profile.operator}` : ""}{profile.available
            ? ""
            : ` (needs ${profile.package})`}
        </option>
      {/each}
    </select>
  </label>
  <label>
    Task
    <select
      value={choice.task ?? ""}
      {disabled}
      onfocus={onopen}
      onchange={(event) => onchange({ ...choice, task: event.currentTarget.value || null })}
    >
      <option value="">None</option>
      {#each tasks?.tasks ?? [] as task (task.id)}
        <option value={task.id}>{task.name ?? task.id}</option>
      {/each}
    </select>
  </label>
</div>

<style>
  .session {
    display: flex;
    gap: 0.75rem;
    align-items: center;
  }
  label {
    display: flex;
    gap: 0.4rem;
    align-items: center;
    color: var(--muted);
    font-size: var(--text-sm);
  }
  select {
    max-width: 12rem;
    padding: 0.35rem 0.5rem;
    border: 1px solid var(--control-line);
    border-radius: var(--radius);
    background: transparent;
    color: var(--text);
    font: inherit;
  }
</style>
