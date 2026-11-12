<script lang="ts">
  // The visualizer docked below the main view.
  import { shortcutHint } from "../actions/keys";
  import { actionLabel, runAction } from "../actions/registry";
  import { app } from "../state/app.svelte";
  import { findVisualizer } from "../visuals/plugin";
  import VisualCanvas from "../visuals/VisualCanvas.svelte";

  const plugin = $derived(findVisualizer(app.visualChoices.dock));
</script>

{#if plugin}
  <div class="dock">
    <VisualCanvas {plugin} ondblclick={() => runAction("visuals.toggleFull")} />
    <div class="controls">
      <button
        title="{actionLabel('visuals.next')}: showing {plugin.label}"
        onclick={() => runAction("visuals.next", { place: "dock" })}>{plugin.label}</button
      >
      <button
        title="{actionLabel('visuals.toggleFull')}{shortcutHint('visuals.toggleFull')}"
        aria-label={actionLabel("visuals.toggleFull")}
        onclick={() => runAction("visuals.toggleFull")}>⛶</button
      >
      <button
        title="{actionLabel('visuals.toggleDock')}{shortcutHint('visuals.toggleDock')}"
        aria-label={actionLabel("visuals.toggleDock")}
        onclick={() => runAction("visuals.toggleDock")}>✕</button
      >
    </div>
  </div>
{/if}

<style>
  .dock {
    position: relative;
    display: flex;
    flex: 1;
    min-width: 0;
  }

  .controls {
    position: absolute;
    top: var(--space-xs);
    right: var(--space-xs);
    display: flex;
    gap: var(--space-xs);
    opacity: 0;
    transition: opacity 0.15s;
  }

  .dock:hover .controls,
  .controls:focus-within {
    opacity: 1;
  }

  button {
    all: unset;
    padding: 0 var(--space-sm);
    min-width: 1.5em;
    text-align: center;
    border-radius: var(--radius);
    background: var(--color-surface-raised);
    color: var(--color-text);
    font-size: var(--font-size-small);
    cursor: pointer;
  }

  button:hover {
    background: var(--color-hover);
  }

  button:focus-visible {
    outline: 1px solid var(--color-accent);
  }
</style>
