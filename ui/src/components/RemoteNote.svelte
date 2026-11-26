<script lang="ts">
  // Said where something can only be set on the computer that holds the
  // vault -- the phone, or a browser over web access, is a window onto it.
  //
  // These sections used to vanish on a remote client with nothing in their
  // place (`{#if … && !app.remote}` and no else), so somebody looking for
  // web access or the assistant's MCP server on their phone found no trace
  // of it and no word on where it had gone. One sentence, the same in each.

  let {
    what,
  }: {
    /** What is set there, as the start of a sentence: "Web access",
     *  "Screen time". Left out, it says "This". */
    what?: string
  } = $props()
</script>

<p class="hint remote-note">
  {what ?? 'This'} is set up on the computer that holds your vault. Open Settings there to change it.
</p>

<style>
  .remote-note {
    margin: 0;
  }
</style>
