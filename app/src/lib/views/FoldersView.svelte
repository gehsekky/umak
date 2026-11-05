<script lang="ts">
  import { actionLabel, runAction } from "../actions/registry";
  import { countOf } from "../lists/format";
  import { app } from "../state/app.svelte";
</script>

<div class="page">
  <div class="header">
    <h1>{actionLabel("nav.folders")}</h1>
    <span class="spacer"></span>
    <button disabled={app.scan !== null} onclick={() => runAction("library.chooseImport")}>
      {actionLabel("library.chooseImport")}
    </button>
    <button disabled={app.roots.length === 0} onclick={() => runAction("library.export")}>
      {actionLabel("library.export")}
    </button>
    <button
      disabled={app.scan !== null || app.roots.length === 0}
      onclick={() => runAction("library.rescan")}
    >
      {actionLabel("library.rescan")}
    </button>
    <button class="primary" disabled={app.scan !== null} onclick={() => runAction("library.addFolder")}>
      {actionLabel("library.addFolder")}
    </button>
  </div>

  {#each app.roots as root (root.id)}
    <div class="root">
      <div class="name">{root.name}</div>
      <div class="path">{root.path}</div>
      <div class="count">
        {countOf(root.trackCount, "track")}
        {#if root.missingCount > 0}
          <span class="missing">{root.missingCount.toLocaleString()} missing</span>
        {/if}
      </div>
    </div>
  {:else}
    <p class="hint">No folders yet. Your music files are only read, never changed.</p>
  {/each}
</div>

<style>
  .page {
    padding: var(--space-md);
    overflow-y: auto;
  }

  .header {
    display: flex;
    align-items: center;
    gap: var(--space-sm);
    margin-bottom: var(--space-md);
  }

  h1 {
    margin: 0;
    font-size: var(--font-size-title);
    font-weight: 600;
  }

  .spacer {
    flex: 1;
  }

  .root {
    padding: var(--space-sm) var(--space-md);
    margin-bottom: var(--space-sm);
    background: var(--color-surface);
    border: 1px solid var(--color-border);
    border-radius: var(--radius);
  }

  .name {
    font-weight: 600;
  }

  .path,
  .count {
    color: var(--color-text-muted);
    font-size: var(--font-size-small);
    overflow-wrap: anywhere;
  }

  .missing {
    margin-left: var(--space-sm);
    color: var(--color-danger);
  }

  .hint {
    color: var(--color-text-muted);
  }

  button {
    padding: var(--space-xs) var(--space-sm);
    background: var(--color-surface-raised);
    color: var(--color-text);
    border: 1px solid var(--color-border);
    border-radius: var(--radius);
    font: inherit;
    font-size: var(--font-size-small);
    cursor: pointer;
  }

  button:hover:enabled {
    background: var(--color-hover);
  }

  button:disabled {
    cursor: default;
    color: var(--color-text-muted);
  }

  button:focus-visible {
    outline: 1px solid var(--color-accent);
    outline-offset: 1px;
  }

  button.primary:enabled {
    background: var(--color-accent);
    border-color: var(--color-accent);
    color: var(--color-on-accent);
  }
</style>
