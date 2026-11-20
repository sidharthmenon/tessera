<script lang="ts">
  import { ui } from '$lib/store.svelte';

  let value = $state(ui.prompt?.value ?? '');

  function close(result: string | null) {
    const p = ui.prompt;
    ui.prompt = null;
    ui.modal = null;
    p?.done(result?.trim() || null);
  }
</script>

<div class="backdrop">
  <form
    class="dialog"
    onsubmit={(e) => {
      e.preventDefault();
      close(value);
    }}
  >
    <h3>{ui.prompt?.title}</h3>
    <input
      bind:value
      {@attach (el) => {
        el.focus();
        el.select();
      }}
      onkeydown={(e) => {
        if (e.key === 'Escape') close(null);
      }}
    />
    <div class="actions">
      <button type="button" onclick={() => close(null)}>Cancel</button>
      <button type="submit" class="primary">OK</button>
    </div>
  </form>
</div>

<style>
  input {
    width: 100%;
    padding: 5px 7px;
  }
</style>
