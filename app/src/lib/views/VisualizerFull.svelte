<script lang="ts">
  // The visualizer filling the screen, with what is playing along the bottom. Escape or
  // F leaves it, and the player's shortcuts keep working.
  import { actionLabel, runAction } from "../actions/registry";
  import { setFullScreen } from "../platform/window";
  import { player } from "../player/state.svelte";
  import { app } from "../state/app.svelte";
  import { findVisualizer } from "../visuals/plugin";
  import VisualCanvas from "../visuals/VisualCanvas.svelte";

  const plugin = $derived(findVisualizer(app.visualChoices.full));
  const track = $derived(
    player.status && player.status.state !== "stopped" ? player.status.track : null,
  );

  $effect(() => {
    if (!app.visualsFull) return;
    void setFullScreen(true);
    return () => void setFullScreen(false);
  });

  function keydown(event: KeyboardEvent): void {
    if (app.visualsFull && event.key === "Escape") {
      app.visualsFull = false;
      event.preventDefault();
    }
  }
</script>

<svelte:window onkeydown={keydown} />

{#if app.visualsFull && plugin}
  <div class="full" role="region" aria-label="Full-screen visualizer">
    <VisualCanvas {plugin} ondblclick={() => runAction("visuals.toggleFull")} />
    <div class="controls">
      <button
        title="{actionLabel('visuals.next')}: showing {plugin.label}"
        onclick={() => runAction("visuals.next", { place: "full" })}>{plugin.label}</button
      >
      <button
        title="Leave full screen (Esc)"
        aria-label="Leave full screen"
        onclick={() => runAction("visuals.toggleFull")}>✕</button
      >
    </div>
    {#if track}
      <div class="now">
        <div class="title">{track.title}</div>
        <div class="detail">{track.artist} - {track.album}</div>
      </div>
    {/if}
  </div>
{/if}

<style>
  .full {
    position: fixed;
    inset: 0;
    z-index: 10;
    display: flex;
    background: var(--color-visual-bg);
  }

  .controls {
    position: absolute;
    top: var(--space-md);
    right: var(--space-md);
    display: flex;
    gap: var(--space-xs);
    opacity: 0;
    transition: opacity 0.3s;
  }

  .full:hover .controls,
  .controls:focus-within {
    opacity: 1;
  }

  button {
    all: unset;
    padding: var(--space-xs) var(--space-sm);
    min-width: 1.5em;
    text-align: center;
    border-radius: var(--radius);
    background: var(--color-surface-raised);
    color: var(--color-text);
    cursor: pointer;
  }

  button:hover {
    background: var(--color-hover);
  }

  button:focus-visible {
    outline: 1px solid var(--color-accent);
  }

  .now {
    position: absolute;
    left: var(--space-lg);
    bottom: var(--space-lg);
    max-width: calc(100% - 2 * var(--space-lg));
    pointer-events: none;
  }

  .title {
    font-size: var(--font-size-title);
    color: var(--color-text);
  }

  .detail {
    color: var(--color-text-muted);
  }

  .title,
  .detail {
    overflow: hidden;
    white-space: nowrap;
    text-overflow: ellipsis;
  }
</style>
