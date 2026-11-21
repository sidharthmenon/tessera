<script lang="ts">
  import { newShow, openShow, redo, saveShow, ui, undo } from '$lib/store.svelte';

  const name = $derived(ui.path ? ui.path.split(/[\\/]/).at(-1) : 'Untitled show');
</script>

<header class="toolbar">
  <span class="logo">TESSERA</span>
  <div class="group">
    <button onclick={() => newShow()}>New</button>
    <button onclick={() => openShow()}>Open…</button>
    <button onclick={() => saveShow()} title="⌘S">Save</button>
    <button onclick={() => saveShow(true)} title="⇧⌘S">Save As…</button>
  </div>
  <div class="group">
    <button onclick={() => undo()} title="⌘Z">Undo</button>
    <button onclick={() => redo()} title="⇧⌘Z">Redo</button>
  </div>
  <button onclick={() => (ui.modal = 'screens')}>Screen Management…</button>
  <span class="name" title={ui.path ? `${ui.path}\nChanges are saved here automatically (2 s after each edit)` : 'Save the show once and every change is then saved to its file automatically'}>{name}</span>
</header>

<style>
  .toolbar {
    grid-area: tool;
    display: flex;
    align-items: center;
    gap: 14px;
    padding: 5px 10px;
    background: var(--panel);
    border-bottom: 1px solid var(--border);
  }
  .logo {
    font-weight: 800;
    letter-spacing: 0.25em;
    color: var(--accent);
  }
  .group {
    display: flex;
    gap: 4px;
  }
  .name {
    margin-left: auto;
    color: var(--muted);
    overflow: hidden;
    text-overflow: ellipsis;
  }
</style>
