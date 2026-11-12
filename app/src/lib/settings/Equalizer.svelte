<script lang="ts">
  // The equalizer's bands and presets, the control for its setting.
  import { actionLabel, runAction } from "../actions/registry";
  import { player } from "../player/state.svelte";
  import { latestOnly } from "../player/volume";

  const MAX_BAND_DB = 12;

  const eq = $derived(player.settings?.eq ?? null);
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
</script>

<div class="row">
  <label class="check">
    <input
      type="checkbox"
      checked={eq?.enabled ?? false}
      disabled={!eq}
      aria-label="Use the equalizer"
      onchange={(event) => sendEq({ enabled: event.currentTarget.checked, bands: shownBands })}
    />
    On
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
    onclick={() => matchingPreset && runAction("player.deleteEqPreset", { name: matchingPreset.name })}
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

<style>
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
    cursor: pointer;
  }

  input {
    margin: 0;
    accent-color: var(--color-accent);
  }

  input:focus-visible,
  select:focus-visible,
  button:focus-visible {
    outline: 1px solid var(--color-accent);
    outline-offset: 1px;
  }

  select {
    padding: var(--space-xs) var(--space-sm);
    background: var(--color-surface-raised);
    color: var(--color-text);
    border: 1px solid var(--color-border);
    border-radius: var(--radius);
    font: inherit;
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

  .name {
    flex: 1;
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

  output {
    color: var(--color-text-muted);
    font-size: var(--font-size-small);
    font-variant-numeric: tabular-nums;
    white-space: nowrap;
  }

  .vertical {
    flex: none;
    /* The tokens set the row height; ten of them make a usable slider. */
    height: calc(var(--size-row) * 5);
    writing-mode: vertical-lr;
    direction: rtl;
  }

  .frequency {
    color: var(--color-text-muted);
    font-size: var(--font-size-small);
  }
</style>
