<script lang="ts">
  // The player bar: what is playing, the transport and seek bar, and the volume.
  import { actionLabel, runAction } from "../actions/registry";
  import { formatDuration } from "../lists/format";
  import { player } from "../player/state.svelte";
  import { gainToSlider, sliderToGain } from "../player/volume";
  import { app } from "../state/app.svelte";

  const status = $derived(player.status);
  const isPlaying = $derived(status?.state === "playing");
  const isActive = $derived(status !== null && status.state !== "stopped");
  const duration = $derived(status?.durationMs ?? 0);
  const track = $derived(isActive ? (status?.track ?? null) : null);

  // While a slider is held, it follows the pointer instead of the player.
  let dragPosition = $state<number | null>(null);
  const shownPosition = $derived(dragPosition ?? player.positionMs);
  let dragVolume = $state<number | null>(null);
  const isMuted = $derived(player.settings?.muted ?? false);
  const shownVolume = $derived(
    dragVolume ?? (isMuted ? 0 : gainToSlider(player.settings?.volume ?? 1)),
  );

  function finishSeek(ms: number): void {
    dragPosition = null;
    // Shown at once, instead of jumping back until the engine reports the new place.
    player.positionMs = ms;
    void runAction("player.seek", { ms });
  }
</script>

<div class="bar">
  <div class="now" data-testid="now-playing">
    {#if track}
      <div class="title" title={track.title}>{track.title}</div>
      <div class="detail" title="{track.artist} - {track.album}">
        {track.artist}<span class="album">{" - "}{track.album}</span>
      </div>
    {:else}
      <div class="detail">Nothing playing</div>
    {/if}
  </div>

  <div class="buttons">
    <button
      title="{actionLabel('player.previous')} (Ctrl+Left)"
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
      title="{actionLabel('player.next')} (Ctrl+Right)"
      aria-label={actionLabel("player.next")}
      disabled={!isActive}
      onclick={() => runAction("player.next")}>⏭</button
    >
  </div>

  <div class="position">
    <span class="time">{formatDuration(isActive ? shownPosition : 0)}</span>
    <input
      class="seek"
      type="range"
      min="0"
      max={duration}
      step="100"
      value={isActive ? shownPosition : 0}
      disabled={!isActive || duration === 0}
      title="Shift+Left and Shift+Right skip 5 seconds"
      aria-label="Position"
      oninput={(event) => (dragPosition = event.currentTarget.valueAsNumber)}
      onchange={(event) => finishSeek(event.currentTarget.valueAsNumber)}
    />
    <span class="time">{formatDuration(isActive ? duration : 0)}</span>
  </div>

  <div class="extras">
    <button
      class:on={isMuted}
      title="{actionLabel('player.toggleMute')} (M)"
      aria-label={actionLabel("player.toggleMute")}
      aria-pressed={isMuted}
      onclick={() => runAction("player.toggleMute")}>{isMuted ? "🔇" : "🔊"}</button
    >
    <input
      class="volume"
      type="range"
      min="0"
      max="1"
      step="0.01"
      value={shownVolume}
      disabled={!player.settings}
      aria-label={actionLabel("player.setVolume")}
      title="{actionLabel('player.setVolume')} {Math.round(shownVolume * 100)}% (Ctrl+Up and Ctrl+Down)"
      oninput={(event) => {
        dragVolume = event.currentTarget.valueAsNumber;
        player.setVolumeSoon(sliderToGain(dragVolume));
      }}
      onchange={() => (dragVolume = null)}
    />
    <button
      class:on={app.queueOpen}
      title="{actionLabel('queue.togglePanel')} (Ctrl+Q)"
      aria-label={actionLabel("queue.togglePanel")}
      aria-pressed={app.queueOpen}
      onclick={() => runAction("queue.togglePanel")}>☰</button
    >
  </div>
</div>

<style>
  .bar {
    display: grid;
    grid-template-columns: minmax(0, 1fr) minmax(0, 2fr) minmax(0, 1fr);
    grid-template-areas:
      "now buttons extras"
      "now position extras";
    column-gap: var(--space-md);
    align-items: center;
    padding: var(--space-xs) var(--space-md);
  }

  .now {
    grid-area: now;
    min-width: 0;
  }

  .buttons {
    grid-area: buttons;
    display: flex;
    justify-content: center;
    gap: var(--space-xs);
  }

  .position {
    grid-area: position;
    display: flex;
    align-items: center;
    gap: var(--space-sm);
    min-width: 0;
  }

  .extras {
    grid-area: extras;
    display: flex;
    align-items: center;
    justify-content: flex-end;
    gap: var(--space-xs);
    min-width: 0;
  }

  /* Too narrow for three columns, so the seek bar gets its own row. */
  @media (max-width: 800px) {
    .bar {
      grid-template-columns: minmax(0, 1fr) auto auto;
      grid-template-areas:
        "now buttons extras"
        "position position position";
    }

    .album {
      display: none;
    }
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

  .on {
    color: var(--color-accent);
  }

  .main {
    background: var(--color-accent);
    color: var(--color-on-accent);
  }

  .main:hover:enabled {
    background: var(--color-accent);
  }

  .title,
  .detail {
    overflow: hidden;
    white-space: nowrap;
    text-overflow: ellipsis;
  }

  .detail,
  .time {
    color: var(--color-text-muted);
    font-size: var(--font-size-small);
  }

  .time {
    font-variant-numeric: tabular-nums;
  }

  input {
    margin: 0;
    accent-color: var(--color-accent);
    min-width: 0;
  }

  .seek {
    flex: 1;
  }

  .volume {
    flex: 0 1 6em;
    width: 6em;
  }
</style>
