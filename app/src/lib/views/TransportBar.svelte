<script lang="ts">
  // Minimal transport controls for phase 1. The full player bar comes with phase 2.
  import { actionLabel, runAction } from "../actions/registry";
  import { formatDuration } from "../lists/format";
  import { player } from "../player/state.svelte";

  const status = $derived(player.status);
  const isPlaying = $derived(status?.state === "playing");
  const isActive = $derived(status !== null && status.state !== "stopped");
  const duration = $derived(status?.durationMs ?? 0);

  // While the handle is held, it follows the pointer instead of the playback.
  let dragValue = $state<number | null>(null);
  const shownPosition = $derived(dragValue ?? player.positionMs);
</script>

<div class="bar">
  <div class="buttons">
    <button
      title={actionLabel("player.previous")}
      aria-label={actionLabel("player.previous")}
      disabled={!isActive}
      onclick={() => runAction("player.previous")}>⏮</button
    >
    <button
      class="main"
      title="{actionLabel('player.toggle')} (Space)"
      aria-label={isPlaying ? "Pause" : "Play"}
      onclick={() => runAction("player.toggle")}>{isPlaying ? "⏸" : "▶"}</button
    >
    <button
      title={actionLabel("player.stop")}
      aria-label={actionLabel("player.stop")}
      disabled={!isActive}
      onclick={() => runAction("player.stop")}>⏹</button
    >
    <button
      title={actionLabel("player.next")}
      aria-label={actionLabel("player.next")}
      disabled={!isActive}
      onclick={() => runAction("player.next")}>⏭</button
    >
  </div>

  <div class="track" data-testid="now-playing">
    {#if isActive && status?.track}
      <div class="title" title={status.track.title}>{status.track.title}</div>
      <div class="artist" title={status.track.artist}>{status.track.artist}</div>
    {:else}
      <div class="artist">Nothing playing</div>
    {/if}
  </div>

  <span class="time">{formatDuration(isActive ? shownPosition : 0)}</span>
  <input
    class="seek"
    type="range"
    min="0"
    max={duration}
    step="100"
    value={isActive ? shownPosition : 0}
    disabled={!isActive || duration === 0}
    aria-label="Position"
    oninput={(event) => (dragValue = event.currentTarget.valueAsNumber)}
    onchange={(event) => {
      const ms = event.currentTarget.valueAsNumber;
      dragValue = null;
      runAction("player.seek", { ms });
    }}
  />
  <span class="time">{formatDuration(isActive ? duration : 0)}</span>
</div>

<style>
  .bar {
    display: grid;
    grid-template-columns: auto minmax(8em, 16em) auto minmax(0, 1fr) auto;
    gap: var(--space-sm);
    align-items: center;
    padding: var(--space-xs) var(--space-md);
  }

  .buttons {
    display: flex;
    gap: var(--space-xs);
  }

  button {
    all: unset;
    width: 2em;
    text-align: center;
    padding: var(--space-xs) 0;
    border-radius: var(--radius);
    cursor: pointer;
  }

  button:hover:enabled {
    background: var(--color-hover);
  }

  button:disabled {
    color: var(--color-text-muted);
    cursor: default;
  }

  button:focus-visible {
    outline: 1px solid var(--color-accent);
  }

  .main {
    background: var(--color-accent);
    color: var(--color-on-accent);
  }

  .main:hover:enabled {
    background: var(--color-accent);
  }

  .track {
    min-width: 0;
  }

  .title,
  .artist {
    overflow: hidden;
    white-space: nowrap;
    text-overflow: ellipsis;
  }

  .artist,
  .time {
    color: var(--color-text-muted);
    font-size: var(--font-size-small);
  }

  .time {
    font-variant-numeric: tabular-nums;
  }

  .seek {
    width: 100%;
    margin: 0;
    accent-color: var(--color-accent);
  }
</style>
