<script lang="ts">
  // Audio settings. The volume itself is on the player bar.
  import { runAction } from "../actions/registry";
  import type { GainCoverage } from "../api/generated/GainCoverage";
  import type { ReplayGainMode } from "../api/generated/ReplayGainMode";
  import { playerClient } from "../player/client";
  import { player } from "../player/state.svelte";
  import { latestOnly } from "../player/volume";
  import { app } from "../state/app.svelte";

  const MAX_PREAMP_DB = 12;

  const settings = $derived(player.settings);
  const isOff = $derived(settings?.replayGain === "off");
  let coverage = $state<GainCoverage | null>(null);
  // While the slider is held, it follows the pointer.
  let draftPreamp = $state<number | null>(null);
  const shownPreamp = $derived(draftPreamp ?? settings?.preampDb ?? 0);

  $effect(() => {
    void app.libraryVersion;
    playerClient.gainCoverage().then(
      (found) => (coverage = found),
      () => (coverage = null),
    );
  });

  const sendGain = latestOnly((args: { replayGain: ReplayGainMode; preampDb: number }) =>
    runAction("player.setGain", args),
  );

  function chooseMode(event: Event & { currentTarget: HTMLSelectElement }): void {
    sendGain({
      replayGain: event.currentTarget.value as ReplayGainMode,
      preampDb: settings?.preampDb ?? 0,
    });
  }

  function movePreamp(event: Event & { currentTarget: HTMLInputElement }): void {
    draftPreamp = event.currentTarget.valueAsNumber;
    sendGain({ replayGain: settings?.replayGain ?? "off", preampDb: draftPreamp });
  }

  function formatDb(db: number): string {
    return `${db > 0 ? "+" : ""}${db.toFixed(1)} dB`;
  }

  /** The tags the chosen mode reads first. A track lacking them uses the other kind. */
  const tagged = $derived.by(() => {
    if (!coverage) return 0;
    return settings?.replayGain === "album" ? coverage.withAlbumGain : coverage.withTrackGain;
  });
</script>

<div class="page">
  <h1>Audio</h1>

  <section>
    <label for="replay-gain">ReplayGain</label>
    <select
      id="replay-gain"
      value={settings?.replayGain ?? "off"}
      disabled={!settings}
      onchange={chooseMode}
    >
      <option value="off">Off</option>
      <option value="album">Album</option>
      <option value="track">Track</option>
    </select>
    <p class="help">
      Evens out loudness using the values in your files' tags. Album keeps the differences
      between songs on an album, and Track evens out every song. Off plays files exactly as they
      are.
    </p>
    {#if coverage && coverage.tracks > 0}
      <p class="help" data-testid="coverage">
        {coverage.withTrackGain.toLocaleString()} of {coverage.tracks.toLocaleString()} tracks have
        track values, and {coverage.withAlbumGain.toLocaleString()} have album values.
        {#if !isOff && tagged < coverage.tracks}
          <strong>Tracks without values play at their full level, so they may be louder than tagged ones.</strong>
        {/if}
      </p>
    {/if}
  </section>

  <section>
    <label for="preamp">Preamp</label>
    <div class="slider">
      <input
        id="preamp"
        type="range"
        min={-MAX_PREAMP_DB}
        max={MAX_PREAMP_DB}
        step="0.5"
        value={shownPreamp}
        disabled={!settings || isOff}
        oninput={movePreamp}
        onchange={() => (draftPreamp = null)}
      />
      <output for="preamp">{formatDb(shownPreamp)}</output>
    </div>
    <p class="help">
      Added on top of ReplayGain, including on tracks without values. It does not apply when
      ReplayGain is off.
    </p>
  </section>
</div>

<style>
  .page {
    padding: var(--space-md) var(--space-lg);
    overflow-y: auto;
    max-width: 40em;
  }

  h1 {
    margin: 0 0 var(--space-md);
    font-size: var(--font-size-title);
    font-weight: 600;
  }

  section {
    margin-bottom: var(--space-lg);
  }

  label {
    display: block;
    margin-bottom: var(--space-xs);
    font-weight: 600;
  }

  select {
    padding: var(--space-xs) var(--space-sm);
    background: var(--color-surface-raised);
    color: var(--color-text);
    border: 1px solid var(--color-border);
    border-radius: var(--radius);
    font: inherit;
  }

  select:focus-visible,
  input:focus-visible {
    outline: 1px solid var(--color-accent);
    outline-offset: 1px;
  }

  .slider {
    display: flex;
    align-items: center;
    gap: var(--space-sm);
  }

  input {
    flex: 1;
    max-width: 24em;
    margin: 0;
    accent-color: var(--color-accent);
  }

  output {
    color: var(--color-text-muted);
    font-size: var(--font-size-small);
    font-variant-numeric: tabular-nums;
    white-space: nowrap;
  }

  .help {
    margin: var(--space-sm) 0 0;
    color: var(--color-text-muted);
    font-size: var(--font-size-small);
  }

  strong {
    color: var(--color-text);
    font-weight: 400;
  }
</style>
