<script lang="ts">
  // Audio settings. The volume itself is on the player bar.
  import { actionLabel, runAction } from "../actions/registry";
  import type { GainCoverage } from "../api/generated/GainCoverage";
  import type { ReplayGainMode } from "../api/generated/ReplayGainMode";
  import { playerClient } from "../player/client";
  import { player } from "../player/state.svelte";
  import { latestOnly } from "../player/volume";
  import { app } from "../state/app.svelte";

  const MAX_PREAMP_DB = 12;
  const MAX_BAND_DB = 12;

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

  const eq = $derived(settings?.eq ?? null);
  const presets = $derived(player.eqPresets?.presets ?? []);
  const frequencies = $derived(player.eqPresets?.frequencies ?? []);
  // While a band's slider is held, the bands follow the pointer.
  let draftBands = $state<number[] | null>(null);
  const shownBands = $derived(draftBands ?? eq?.bands ?? []);
  const sendEq = latestOnly((args: { enabled: boolean; bands: number[] }) =>
    runAction("player.setEq", args),
  );

  const matchingPreset = $derived(
    presets.find((preset) => preset.bands.every((db, band) => db === shownBands[band])),
  );
  let savingPreset = $state(false);
  let presetName = $state("");
  let presetInput = $state<HTMLInputElement>();

  $effect(() => {
    presetInput?.focus();
  });

  function moveBand(band: number, db: number): void {
    const bands = [...shownBands];
    bands[band] = db;
    draftBands = bands;
    sendEq({ enabled: eq?.enabled ?? true, bands });
  }

  function choosePreset(event: Event & { currentTarget: HTMLSelectElement }): void {
    const preset = presets.find((candidate) => candidate.name === event.currentTarget.value);
    if (preset) sendEq({ enabled: eq?.enabled ?? true, bands: preset.bands });
  }

  async function savePreset(event: SubmitEvent): Promise<void> {
    event.preventDefault();
    const name = presetName.trim();
    if (name && (await runAction("player.saveEqPreset", { name, bands: shownBands }))) {
      savingPreset = false;
      presetName = "";
    }
  }

  function formatFrequency(hertz: number): string {
    return hertz >= 1000 ? `${hertz / 1000}k` : String(hertz);
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

  <section>
    <div class="row">
      <label class="check">
        <input
          type="checkbox"
          checked={eq?.enabled ?? false}
          disabled={!eq}
          aria-label="Use the equalizer"
          onchange={(event) =>
            sendEq({ enabled: event.currentTarget.checked, bands: shownBands })}
        />
        Equalizer
      </label>
      <span class="spacer"></span>
      <select
        aria-label="Equalizer preset"
        value={matchingPreset?.name ?? ""}
        disabled={!eq}
        onchange={choosePreset}
      >
        {#if !matchingPreset}<option value="">Custom</option>{/if}
        {#each presets as preset (preset.name)}
          <option value={preset.name}>{preset.name}</option>
        {/each}
      </select>
      <button onclick={() => (savingPreset = !savingPreset)} disabled={!eq}>
        {actionLabel("player.saveEqPreset")}
      </button>
      <button
        disabled={!matchingPreset || matchingPreset.builtIn}
        onclick={() =>
          matchingPreset && runAction("player.deleteEqPreset", { name: matchingPreset.name })}
      >
        {actionLabel("player.deleteEqPreset")}
      </button>
    </div>

    {#if savingPreset}
      <form class="row" onsubmit={savePreset}>
        <input
          bind:this={presetInput}
          bind:value={presetName}
          class="name"
          placeholder="Preset name"
          aria-label="Name for the preset"
          onkeydown={(event) => {
            if (event.key === "Escape") savingPreset = false;
          }}
        />
        <button class="primary" type="submit" disabled={!presetName.trim()}>Save</button>
      </form>
    {/if}

    <div class="bands" class:off={!eq?.enabled}>
      {#each frequencies as frequency, band (frequency)}
        <div class="band">
          <output>{(shownBands[band] ?? 0) > 0 ? "+" : ""}{shownBands[band] ?? 0}</output>
          <input
            class="vertical"
            type="range"
            min={-MAX_BAND_DB}
            max={MAX_BAND_DB}
            step="0.5"
            value={shownBands[band] ?? 0}
            disabled={!eq?.enabled}
            aria-label="{formatFrequency(frequency)} hertz, in decibels"
            oninput={(event) => moveBand(band, event.currentTarget.valueAsNumber)}
            onchange={() => (draftBands = null)}
          />
          <span class="frequency">{formatFrequency(frequency)}</span>
        </div>
      {/each}
    </div>
    <p class="help">
      Boosting a band lowers everything by the same amount to make room, and a limiter catches any
      peaks left, so nothing clips. When the equalizer is off and ReplayGain is off, files play
      exactly as they are.
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

  .row {
    display: flex;
    align-items: center;
    gap: var(--space-xs);
    margin-bottom: var(--space-sm);
    flex-wrap: wrap;
  }

  .spacer {
    flex: 1;
  }

  .check {
    display: flex;
    align-items: center;
    gap: var(--space-xs);
    margin: 0;
    cursor: pointer;
  }

  .check input {
    flex: none;
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
    color: var(--color-text-muted);
    cursor: default;
  }

  button.primary {
    background: var(--color-accent);
    border-color: var(--color-accent);
    color: var(--color-on-accent);
  }

  button:focus-visible {
    outline: 1px solid var(--color-accent);
    outline-offset: 1px;
  }

  .name {
    max-width: none;
    padding: var(--space-xs) var(--space-sm);
    background: var(--color-surface);
    color: var(--color-text);
    border: 1px solid var(--color-border);
    border-radius: var(--radius);
    font: inherit;
  }

  .bands {
    display: flex;
    justify-content: space-between;
    gap: var(--space-xs);
    max-width: 32em;
  }

  .bands.off {
    opacity: 0.5;
  }

  .band {
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: var(--space-xs);
    flex: 1;
    min-width: 0;
  }

  .vertical {
    flex: none;
    /* The tokens set the row height; ten of them make a usable slider. */
    height: calc(var(--size-row) * 5);
    width: auto;
    max-width: none;
    writing-mode: vertical-lr;
    direction: rtl;
  }

  .frequency {
    color: var(--color-text-muted);
    font-size: var(--font-size-small);
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
