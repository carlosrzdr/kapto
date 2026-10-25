<script lang="ts">
  import type { RobotJoint, RobotOverview, RobotStream, StreamValues } from "./api-types";
  import { breakable, sentence } from "./format";

  interface Props {
    robot: RobotOverview;
    /** Each stream's newest decoded values, by stream name. */
    values?: StreamValues[];
  }

  let { robot, values = [] }: Props = $props();

  /** Values older than this are shown as stale: the stream has gone quiet. */
  const STALE_MS = 2000;

  /** The value a reference like `arm_joints.joint_positions[2]` names, if known and fresh. */
  function current(reference: string | null | undefined): number | null {
    const parts = reference?.match(/^([^.]+)\.([^[\]]+)(?:\[(\d+)\])?$/);
    if (!parts) {
      return null;
    }
    const [, streamName, fieldName, index] = parts;
    const stream = robot.streams.find((candidate) => candidate.name === streamName);
    const newest = values.find((candidate) => candidate.stream === streamName);
    if (!stream || !newest || newest.age_ms > STALE_MS) {
      return null;
    }
    let offset = 0;
    for (const field of stream.fields) {
      if (field.name === fieldName) {
        return newest.values[offset + Number(index ?? 0)] ?? null;
      }
      offset += field.count;
    }
    return null;
  }

  /** Where a value sits in a joint's range, from 0 to 1, if the joint has one. */
  function position(joint: RobotJoint, value: number): number | null {
    if (joint.lower == null || joint.upper == null || joint.upper <= joint.lower) {
      return null;
    }
    return Math.min(1, Math.max(0, (value - joint.lower) / (joint.upper - joint.lower)));
  }

  function shown(joint: RobotJoint, value: number): string {
    return joint.kind === "prismatic" ? `${(value * 1000).toFixed(1)} mm` : `${((value * 180) / Math.PI).toFixed(1)}°`;
  }

  const moving = $derived(robot.joints.filter((joint) => joint.kind !== "fixed"));

  /** A joint's range in its own units: degrees for turning joints, mm for sliding ones. */
  function range(joint: RobotJoint): string {
    if (joint.lower == null || joint.upper == null) {
      return joint.kind === "continuous" ? "unlimited" : "–";
    }
    const [scale, unit] = joint.kind === "prismatic" ? [1000, " mm"] : [180 / Math.PI, "°"];
    return `${(joint.lower * scale).toFixed(0)} … ${(joint.upper * scale).toFixed(0)}${unit}`;
  }

  /** What a stream decodes into, e.g. `struct: joint_positions[7] rad, gripper m`. */
  function decoded(stream: RobotStream): string {
    if (!stream.decoder) {
      return "recorded only";
    }
    const fields = stream.fields.map(
      (field) => `${field.name}${field.count > 1 ? `[${field.count}]` : ""}${field.unit ? ` ${field.unit}` : ""}`,
    );
    return `${stream.decoder}: ${fields.join(", ")}`;
  }

  function driver(joint: RobotJoint): string {
    if (joint.mimics) {
      return `follows ${joint.mimics}`;
    }
    return joint.position_from ?? "not mapped";
  }
</script>

<p class="summary">
  <strong>{robot.name}</strong> · {robot.links} links · {moving.length} moving joints · {robot.streams.length}
  {robot.streams.length === 1 ? "stream" : "streams"}
</p>

{#if robot.warnings.length > 0}
  <ul class="warnings">
    {#each robot.warnings as warning, index (index)}
      <li>{sentence(warning.message)} <span class="remedy">{warning.remedy}</span></li>
    {/each}
  </ul>
{/if}

<h2>Joints</h2>
<table aria-label="Joints">
  <thead>
    <tr>
      <th scope="col">Joint and what drives it</th>
      <th scope="col">Now</th>
      <th scope="col">Range</th>
    </tr>
  </thead>
  <tbody>
    {#each moving as joint (joint.name)}
      <!-- A joint that copies another is shown by the one it copies. -->
      {@const value = current(joint.position_from)}
      <tr>
        <!-- What drives a joint is there on demand; one nothing drives says so, being why it doesn't move. -->
        <th scope="row" title={joint.position_from || joint.mimics ? `Driven by ${driver(joint)}` : undefined}>
          {joint.name}
          {#if !joint.position_from && !joint.mimics}
            <span class="source problem">{driver(joint)}</span>
          {:else}
            <span class="visually-hidden">{` · ${driver(joint)}`}</span>
          {/if}
        </th>
        <td class="now">
          {#if value !== null}
            {@const fraction = position(joint, value)}
            <span class:problem={fraction === 0 || fraction === 1}>{shown(joint, value)}</span>
            {#if fraction !== null}
              <meter min="0" max="1" value={fraction} title="Where it is in its range"></meter>
            {/if}
          {:else}
            <span class="unknown">–</span>
          {/if}
        </td>
        <td class="range">{range(joint)}</td>
      </tr>
    {/each}
  </tbody>
</table>

<h2>Streams</h2>
<table aria-label="Declared streams">
  <thead>
    <tr>
      <th scope="col">Stream and input</th>
      <th scope="col">Rate</th>
      <th scope="col">Fields</th>
    </tr>
  </thead>
  <tbody>
    {#each robot.streams as stream (stream.name)}
      <tr>
        <th scope="row">
          {stream.name}
          <span class="source">{stream.input}</span>
        </th>
        <td class="range">{stream.rate_hz != null ? `${stream.rate_hz} Hz` : "–"}</td>
        <td class="fields">{#each breakable(decoded(stream)) as piece, index (index)}{piece}<wbr />{/each}</td>
      </tr>
    {/each}
  </tbody>
</table>

<p class="files" title="The files Kapto was started with">
  {#each breakable(robot.files.urdf) as piece, index (index)}{piece}<wbr />{/each}
</p>

<style>
  .summary {
    margin: 0 0 0.5rem;
  }
  .warnings {
    margin: 0 0 0.5rem;
    padding-left: 1.2rem;
    color: var(--warning);
  }
  .remedy {
    color: var(--muted);
  }
  table {
    width: 100%;
    border-collapse: collapse;
    margin-bottom: 0.75rem;
    font-variant-numeric: tabular-nums;
  }
  th,
  td {
    padding: 0.3rem 0.5rem;
    text-align: left;
    border-bottom: 1px solid var(--line);
    vertical-align: top;
  }
  th[scope="row"] {
    font-weight: 500;
  }
  thead th {
    color: var(--muted);
    font-weight: 500;
    font-size: var(--text-xs);
  }
  .source,
  .fields,
  .files {
    font-family: var(--font-data);
    font-size: var(--text-xs);
    /* Not `anywhere`: that lets the table squeeze a column mid-word. Names
       and paths wrap at their dots, underscores and slashes instead. */
    overflow-wrap: break-word;
  }
  .range {
    white-space: nowrap;
  }
  th .source {
    display: block;
    font-weight: 400;
    font-size: var(--text-xs);
    color: var(--muted);
  }
  th .source.problem {
    color: var(--warning);
  }
  .files {
    margin: 0;
    color: var(--muted);
  }
  .problem {
    color: var(--warning);
  }
  .now {
    white-space: nowrap;
    font-variant-numeric: tabular-nums;
  }
  meter {
    width: 3rem;
    height: 0.6rem;
    margin-left: 0.4rem;
    vertical-align: middle;
  }
  .unknown {
    color: var(--muted);
  }
</style>
