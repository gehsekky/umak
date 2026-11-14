<script lang="ts">
  // Switches the library between its songs, its artists and its albums.
  import { actionLabel, runAction } from "../actions/registry";
  import { app } from "../state/app.svelte";

  const TABS = [
    { browse: "library", action: "nav.library" },
    { browse: "artists", action: "nav.artists" },
    { browse: "albums", action: "nav.albums" },
  ] as const;
</script>

<div class="tabs" role="group" aria-label="Show the library by">
  {#each TABS as tab (tab.browse)}
    <button
      class:on={app.browse === tab.browse}
      aria-pressed={app.browse === tab.browse}
      onclick={() => runAction(tab.action)}
    >
      {actionLabel(tab.action)}
    </button>
  {/each}
</div>

<style>
  .tabs {
    display: flex;
    flex: none;
    border: 1px solid var(--color-border);
    border-radius: var(--radius);
    overflow: hidden;
  }

  button {
    padding: var(--space-xs) var(--space-md);
    background: var(--color-surface-raised);
    color: var(--color-text);
    border: none;
    font: inherit;
    font-size: var(--font-size-small);
    white-space: nowrap;
    cursor: pointer;
  }

  button + button {
    border-left: 1px solid var(--color-border);
  }

  button:hover {
    background: var(--color-hover);
  }

  button.on {
    background: var(--color-accent);
    color: var(--color-on-accent);
  }

  button:focus-visible {
    outline: 1px solid var(--color-accent);
    outline-offset: -2px;
  }
</style>
