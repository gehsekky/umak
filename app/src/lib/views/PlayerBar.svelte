<script lang="ts">
  // The player bar: what is playing, the transport and seek bar, and the volume.
  import { shortcutHint, shortcutOf } from "../actions/keys";
  import { actionLabel, runAction } from "../actions/registry";
  import { keyLabel } from "../actions/shortcuts";
  import Cover from "../lists/Cover.svelte";
  import { formatDuration } from "../lists/format";
  import StarRating from "../lists/StarRating.svelte";
  import { player } from "../player/state.svelte";
  import WaveformSeek from "../player/WaveformSeek.svelte";
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

  const skipKeys = $derived(
    [shortcutOf("player.skipBack"), shortcutOf("player.skipForward")].flatMap((key) =>
      key ? [keyLabel(key)] : [],
    ),
  );
  const skipTitle = $derived(
    skipKeys.length > 0
      ? `${skipKeys.join(" and ")} ${skipKeys.length === 1 ? "skips" : "skip"} 5 seconds`
      : undefined,
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
      <div class="art">
        <Cover art={player.art?.art ?? null} size="small" alt="Cover of {track.album}" />
      </div>
      <div class="text">
        <!-- Beside the title, so a track starting does not make the bar taller. -->
        <div class="heading">
          <div class="title" title={track.title}>{track.title}</div>
          <StarRating
            focusable
            rating={track.rating}
            label="Rating of {track.title}"
            onrate={(rating) => runAction("tracks.rate", { trackIds: [track.id], rating })}
          />
        </div>
        <div class="detail" title="{track.artist} - {track.album}">
          {track.artist}<span class="album">{" - "}{track.album}</span>
        </div>
      </div>
    {:else}
      <div class="detail">Nothing playing</div>
    {/if}
  </div>

  <div class="buttons">
    <button
      title="{actionLabel('player.previous')}{shortcutHint('player.previous')}"
      aria-label={actionLabel("player.previous")}
      disabled={!isActive}
      onclick={() => runAction("player.previous")}>⏮</button
    >
    <button
      class="main"
      title="{actionLabel('player.toggle')}{shortcutHint('player.toggle')}"
      aria-label={isPlaying ? "Pause" : "Play"}
      onclick={() => runAction("player.toggle")}>{isPlaying ? "⏸" : "▶"}</button
    >
    <button
      title="{actionLabel('player.stop')}{shortcutHint('player.stop')}"
      aria-label={actionLabel("player.stop")}
      disabled={!isActive}
      onclick={() => runAction("player.stop")}>⏹</button
    >
    <button
      title="{actionLabel('player.next')}{shortcutHint('player.next')}"
      aria-label={actionLabel("player.next")}
      disabled={!isActive}
      onclick={() => runAction("player.next")}>⏭</button
    >
  </div>

  <div class="position">
    <span class="time">{formatDuration(isActive ? shownPosition : 0)}</span>
    <WaveformSeek
      peaks={isActive ? (player.waveform?.peaks ?? []) : []}
      levels={isActive ? (player.waveform?.levels ?? []) : []}
      positionMs={isActive ? shownPosition : 0}
      durationMs={duration}
      disabled={!isActive || duration === 0}
      title={skipTitle}
      oninput={(ms) => (dragPosition = ms)}
      onchange={finishSeek}
    />
    <span class="time">{formatDuration(isActive ? duration : 0)}</span>
  </div>

  <div class="extras">
    <button
      class:on={isMuted}
      title="{actionLabel('player.toggleMute')}{shortcutHint('player.toggleMute')}"
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
      title="{actionLabel('player.setVolume')} {Math.round(shownVolume * 100)}%{shortcutHint('player.volumeUp', 'player.volumeDown')}"
      oninput={(event) => {
        dragVolume = event.currentTarget.valueAsNumber;
        player.setVolumeSoon(sliderToGain(dragVolume));
      }}
      onchange={() => (dragVolume = null)}
    />
    <button
      class:on={app.visualsDocked}
      title="{actionLabel('visuals.toggleDock')}{shortcutHint('visuals.toggleDock')}"
      aria-label={actionLabel("visuals.toggleDock")}
      aria-pressed={app.visualsDocked}
      onclick={() => runAction("visuals.toggleDock")}>〰</button
    >
    <button
      title="{actionLabel('visuals.toggleFull')}{shortcutHint('visuals.toggleFull')}"
      aria-label={actionLabel("visuals.toggleFull")}
      onclick={() => runAction("visuals.toggleFull")}>⛶</button
    >
    <button
      class:on={app.queueOpen}
      title="{actionLabel('queue.togglePanel')}{shortcutHint('queue.togglePanel')}"
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
    display: flex;
    align-items: center;
    gap: var(--space-sm);
  }

  .art {
    flex: none;
    width: var(--size-player-art);
    height: var(--size-player-art);
  }

  .text {
    min-width: 0;
  }

  .heading {
    display: flex;
    align-items: baseline;
    gap: var(--space-sm);
  }

  .heading :global(.stars) {
    flex: none;
    font-size: var(--font-size-small);
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

  .volume {
    flex: 0 1 6em;
    width: 6em;
  }
</style>
