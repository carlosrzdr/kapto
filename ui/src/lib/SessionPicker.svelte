<script lang="ts">
  import type { SessionChoice } from "./api";
  import type { ProfilesOverview, TasksOverview } from "./api-types";
  import Choice from "./controls/Choice.svelte";

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
  <Choice
    label="Profile"
    value={choice.profile ?? ""}
    {disabled}
    width="wide"
    options={[
      { value: "", label: "None" },
      ...(profiles?.profiles ?? []).map((profile) => ({
        value: profile.id,
        label: profile.id,
        detail: profile.available
          ? (profile.operator ?? undefined)
          : `Needs the package ${profile.package}`,
        disabled: !profile.available,
      })),
    ]}
    {onopen}
    onchange={(value) => onchange({ ...choice, profile: value || null })}
  />
  <Choice
    label="Task"
    value={choice.task ?? ""}
    {disabled}
    width="wide"
    options={[
      { value: "", label: "None" },
      ...(tasks?.tasks ?? []).map((task) => ({ value: task.id, label: task.name ?? task.id, detail: task.name ? task.id : undefined })),
    ]}
    {onopen}
    onchange={(value) => onchange({ ...choice, task: value || null })}
  />
</div>

<style>
  .session {
    display: flex;
    gap: var(--space-3);
    align-items: center;
  }
</style>
