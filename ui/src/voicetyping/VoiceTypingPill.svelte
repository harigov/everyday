<script lang="ts">
  // Voice typing's paste window. Never seen: on Wayland only a window that has
  // the keyboard may set the clipboard, so this one is shown for the moment a
  // dictation is typed and hidden again -- see
  // `crates/karmayoga-app/src/voicetyping/paste.rs`. What voice typing shows
  // while it listens is a card in the panel (`src/panel/`).
</script>

<style>
  :global(html),
  :global(body) {
    margin: 0;
    background: transparent;
    overflow: hidden;
  }
</style>
