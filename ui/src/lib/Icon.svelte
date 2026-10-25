<script lang="ts" module>
  /** The icons there are: one set, drawn on a 24px grid in one stroke weight. */
  export type IconName = "play" | "pause" | "record" | "stop" | "frame" | "cube" | "chevron" | "attention" | "keyboard" | "list";
</script>

<script lang="ts">
  interface Props {
    name: IconName;
    /**
     * What it means, said to screen readers, when nothing beside it says
     * so already; without one, it is decoration and hidden from them.
     */
    label?: string;
  }

  let { name, label }: Props = $props();
</script>

<svg
  class="icon"
  viewBox="0 0 24 24"
  aria-hidden={label ? undefined : "true"}
  role={label ? "img" : undefined}
  aria-label={label}
  focusable="false"
>
  {#if name === "play"}
    <path class="filled" d="M7 5.5v13l11-6.5z" />
  {:else if name === "pause"}
    <path class="filled" d="M7 5h3.5v14H7zM13.5 5H17v14h-3.5z" />
  {:else if name === "record"}
    <circle class="filled" cx="12" cy="12" r="6" />
  {:else if name === "stop"}
    <rect class="filled" x="6.5" y="6.5" width="11" height="11" rx="1.5" />
  {:else if name === "frame"}
    <path d="M4 9V5h4M16 5h4v4M20 15v4h-4M8 19H4v-4" />
    <circle cx="12" cy="12" r="2.5" />
  {:else if name === "cube"}
    <path d="M12 3.5l7.5 4.25v8.5L12 20.5l-7.5-4.25v-8.5zM12 12l7.5-4.25M12 12v8.5M12 12L4.5 7.75" />
  {:else if name === "chevron"}
    <path d="M9 6l6 6-6 6" />
  {:else if name === "attention"}
    <circle class="filled" cx="12" cy="12" r="5" />
  {:else if name === "keyboard"}
    <rect x="2.5" y="6" width="19" height="12" rx="2" />
    <path d="M6.5 10h.01M10 10h.01M14 10h.01M17.5 10h.01M8 14h8" />
  {:else if name === "list"}
    <path d="M9 6.5h11M9 12h11M9 17.5h11M4.5 6.5h.01M4.5 12h.01M4.5 17.5h.01" />
  {/if}
</svg>

<style>
  .icon {
    flex: none;
    width: 1em;
    height: 1em;
    vertical-align: -0.125em;
    fill: none;
    stroke: currentColor;
    stroke-width: 2;
    stroke-linecap: round;
    stroke-linejoin: round;
  }
  .filled {
    fill: currentColor;
    stroke: none;
  }
</style>
