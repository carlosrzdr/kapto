<script lang="ts">
  import { breakable } from "./format";

  interface Props {
    /** A name, code, path or a message naming them: it wraps only between their parts. */
    text: string;
  }

  let { text }: Props = $props();
</script>

{#each breakable(text) as piece, index (index)}{piece}<wbr />{/each}
