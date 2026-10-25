<script lang="ts">
  import type { ProcessStatus } from "./api-types";
  import { bytes, cores, duration } from "./format";
  import { processAnchor } from "./health";

  interface Props {
    processes: ProcessStatus[];
  }

  let { processes }: Props = $props();

  function state(process: ProcessStatus): string {
    switch (process.state) {
      case "running":
        return `up ${duration(process.uptime_s ?? 0)}`;
      case "restarting":
        return `restarting in ${duration(process.restart_in_s ?? 0)}`;
      case "starting":
        return "starting";
      case "stopped":
        return "stopped";
    }
  }

  const unknown = "–";
</script>

<table>
  <thead>
    <tr>
      <th scope="col">Process</th>
      <th scope="col">State</th>
      <th scope="col" title="In cores: 1.00 is one CPU core kept busy, of however many the machine has">CPU</th>
      <th scope="col">Memory</th>
      <th scope="col">Threads</th>
      <th scope="col">Restarts</th>
    </tr>
  </thead>
  <tbody>
    {#each processes as process (process.name)}
      <!-- Opened by the header's health chip when it is what needs attention. -->
      <tr id={processAnchor(process.name)} class="anchor" tabindex="-1">
        <th scope="row">{process.name}</th>
        <td class:problem={process.state === "restarting"} title={process.last_exit ? `Last ended: ${process.last_exit}` : undefined}>
          {state(process)}
        </td>
        <td>{process.cpu_cores != null ? cores(process.cpu_cores) : unknown}</td>
        <td>{process.memory_bytes != null ? bytes(process.memory_bytes) : unknown}</td>
        <td>{process.threads ?? unknown}</td>
        <td class:problem={process.restarts > 0} class:quiet={process.restarts === 0}>{process.restarts}</td>
      </tr>
      {#if process.state === "restarting" && process.last_exit}
        <tr class="exit">
          <td colspan="6">{process.name} ended: {process.last_exit}</td>
        </tr>
      {/if}
    {/each}
  </tbody>
</table>

<style>
  table {
    width: 100%;
    border-collapse: collapse;
    font-variant-numeric: tabular-nums;
  }
  th,
  td {
    padding: 0.35rem 0.5rem;
    text-align: right;
    border-bottom: 1px solid var(--line);
  }
  th[scope="row"],
  thead th:first-child {
    text-align: left;
    font-weight: 500;
  }
  thead th {
    color: var(--muted);
    font-weight: 500;
    font-size: var(--text-xs);
  }
  .problem {
    color: var(--warning);
    font-weight: 600;
  }
  .quiet {
    color: var(--muted);
  }
  .exit td {
    text-align: left;
    color: var(--warning);
    font-size: var(--text-sm);
  }
</style>
