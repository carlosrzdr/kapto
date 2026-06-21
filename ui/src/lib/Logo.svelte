<!--
  Kapto's mark: the K of Fira Sans ExtraBold, its arm replaced by the
  record dot. Decoration beside the name, so hidden from screen readers.
  The same drawing as docs/assets/kapto-mark-*.svg and public/favicon.svg.
-->
<script lang="ts">
  // Nothing to set: it takes its colours from the text around it and the accent.
</script>

<svg class="logo" viewBox="-49.5 0 707 707" aria-hidden="true" focusable="false">
  <path class="k" d="M0 12H185V707H0Z M381 322L608 707H396L191 337Z" />
  <circle class="dot" cx="439.9" cy="122.1" r="122.1" />
</svg>

<style>
  .logo {
    width: 1.6em;
    height: 1.6em;
    flex: none;
  }
  .k {
    fill: currentColor;
  }
  .dot {
    fill: var(--accent);
  }
</style>
