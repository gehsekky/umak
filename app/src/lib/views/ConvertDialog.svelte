<script lang="ts">
  import { untrack } from "svelte";
  import { actionLabel, runAction } from "../actions/registry";
  import { call } from "../api/client";
  import type { ConvertCodec } from "../api/generated/ConvertCodec";
  import type { ConvertPlan } from "../api/generated/ConvertPlan";
  import type { ConvertPreset } from "../api/generated/ConvertPreset";
  import type { ConvertSettings } from "../api/generated/ConvertSettings";
  import {
    applyPreset,
    BIT_DEPTHS,
    BITRATE_RANGES,
    CODECS,
    describePlan,
    formatRate,
    isLossless,
    loadChoices,
    matchingPreset,
    MP3_BITRATES,
    sampleRatesFor,
    saveChoices,
    withCodec,
  } from "../convert/options";
  import { countOf } from "../lists/format";
  import { pickFolder } from "../platform/dialog";
  import { app } from "../state/app.svelte";

  /** How long the form waits after a change before planning again. */
  const PLAN_DELAY_MS = 250;
  const THREAD_CHOICES = [1, 2, 3, 4, 6, 8, 12, 16];
  const DEFAULT_SETTINGS: ConvertSettings = {
    codec: "mp3",
    mp3Quality: 0,
    bitrateKbps: null,
    maxSampleRate: null,
    maxBitDepth: null,
  };

  let dialog = $state<HTMLDialogElement>();
  let presets = $state.raw<ConvertPreset[]>([]);
  let settings = $state.raw<ConvertSettings>(DEFAULT_SETTINGS);
  let custom = $state(false);
  let folder = $state("");
  let threads = $state<number | null>(null);
  let addToLibrary = $state(false);
  let plan = $state.raw<ConvertPlan | null>(null);
  let planError = $state<string | null>(null);
  let planning = $state(false);
  let starting = $state(false);

  const trackIds = $derived(app.convertTracks);
  const preset = $derived(matchingPreset(presets, settings));
  const lossless = $derived(isLossless(settings.codec));
  const rates = $derived(sampleRatesFor(settings.codec));
  const range = $derived(BITRATE_RANGES[settings.codec]);
  const canAdd = $derived(settings.codec !== "opus");
  const ready = $derived(
    !planning && !starting && plan !== null && planError === null && plan.ready > 0,
  );

  function open(): void {
    const remembered = loadChoices();
    settings = remembered?.settings ?? DEFAULT_SETTINGS;
    folder = remembered?.folder ?? "";
    threads = remembered?.threads ?? null;
    addToLibrary = remembered?.addToLibrary ?? false;
    custom = false;
    plan = null;
    planError = null;
    if (presets.length === 0) {
      call("convert.presets", {})
        .then((list) => {
          presets = list.presets;
          custom = matchingPreset(presets, settings) === undefined;
        })
        .catch((failure: { message?: string }) => app.fail(failure.message ?? String(failure)));
    } else {
      custom = matchingPreset(presets, settings) === undefined;
    }
    dialog?.showModal();
  }

  $effect(() => {
    if (trackIds && dialog && !dialog.open) untrack(open);
    else if (!trackIds && dialog?.open) dialog.close();
  });

  // Plans again whenever what would be converted changes.
  $effect(() => {
    const ids = trackIds;
    const chosen = settings;
    const target = folder.trim();
    if (!ids || !target) {
      plan = null;
      planError = null;
      return;
    }
    planning = true;
    let current = true;
    const timer = setTimeout(() => {
      call("convert.plan", { trackIds: ids, settings: chosen, folder: target })
        .then((result) => {
          if (!current) return;
          plan = result;
          planError = null;
        })
        .catch((failure: { message?: string }) => {
          if (!current) return;
          plan = null;
          planError = failure.message ?? String(failure);
        })
        .finally(() => {
          if (current) planning = false;
        });
    }, PLAN_DELAY_MS);
    return () => {
      current = false;
      clearTimeout(timer);
    };
  });

  function chooseFormat(id: string): void {
    const chosen = presets.find((candidate) => candidate.id === id);
    custom = !chosen;
    if (chosen) settings = applyPreset(settings, chosen);
  }

  function chooseCodec(codec: ConvertCodec): void {
    settings = withCodec(settings, codec);
  }

  function chooseMp3Mode(mode: string): void {
    settings =
      mode === "vbr"
        ? { ...settings, mp3Quality: 0, bitrateKbps: null }
        : { ...settings, mp3Quality: null, bitrateKbps: 320 };
  }

  function optionalNumber(value: string): number | null {
    return value === "" ? null : Number(value);
  }

  async function chooseFolder(): Promise<void> {
    const path = await pickFolder("Choose where the converted files go");
    if (path) folder = path;
  }

  async function start(event: SubmitEvent): Promise<void> {
    event.preventDefault();
    if (!trackIds || !ready) return;
    const request = {
      trackIds,
      settings,
      folder: folder.trim(),
      threads,
      addToLibrary: canAdd && addToLibrary,
    };
    starting = true;
    if (await runAction("convert.start", request)) {
      saveChoices({ settings, folder: request.folder, threads, addToLibrary });
    }
    starting = false;
  }
</script>

<dialog bind:this={dialog} aria-labelledby="convert-title" onclose={() => (app.convertTracks = null)}>
  <form onsubmit={start}>
    <h2 id="convert-title">Convert {countOf(trackIds?.length ?? 0, "track")}</h2>

    <div class="grid">
      <label for="convert-format">Format</label>
      <select
        id="convert-format"
        value={custom ? "custom" : (preset?.id ?? "custom")}
        onchange={(event) => chooseFormat(event.currentTarget.value)}
      >
        {#each presets as choice (choice.id)}
          <option value={choice.id}>{choice.name}</option>
        {/each}
        <option value="custom">Custom</option>
      </select>

      {#if custom}
        <label for="convert-codec">Codec</label>
        <select
          id="convert-codec"
          value={settings.codec}
          onchange={(event) => chooseCodec(event.currentTarget.value as ConvertCodec)}
        >
          {#each CODECS as entry (entry.codec)}
            <option value={entry.codec}>{entry.label}</option>
          {/each}
        </select>

        {#if settings.codec === "mp3"}
          <label for="convert-mp3-mode">Bitrate</label>
          <div class="pair">
            <select
              id="convert-mp3-mode"
              value={settings.mp3Quality !== null ? "vbr" : "cbr"}
              onchange={(event) => chooseMp3Mode(event.currentTarget.value)}
            >
              <option value="vbr">Variable</option>
              <option value="cbr">Constant</option>
            </select>
            {#if settings.mp3Quality !== null}
              <select
                aria-label="Quality"
                value={settings.mp3Quality}
                onchange={(event) =>
                  (settings = { ...settings, mp3Quality: Number(event.currentTarget.value) })}
              >
                {#each Array.from({ length: 10 }, (_, quality) => quality) as quality (quality)}
                  <option value={quality}>V{quality}{quality === 0 ? " (best)" : ""}</option>
                {/each}
              </select>
            {:else}
              <select
                aria-label="Kbps"
                value={settings.bitrateKbps}
                onchange={(event) =>
                  (settings = { ...settings, bitrateKbps: Number(event.currentTarget.value) })}
              >
                {#each MP3_BITRATES as kbps (kbps)}
                  <option value={kbps}>{kbps} kbps</option>
                {/each}
              </select>
            {/if}
          </div>
        {:else if range}
          <label for="convert-bitrate">Bitrate (kbps)</label>
          <input
            id="convert-bitrate"
            type="number"
            min={range.min}
            max={range.max}
            value={settings.bitrateKbps}
            oninput={(event) =>
              (settings = { ...settings, bitrateKbps: optionalNumber(event.currentTarget.value) })}
          />
        {/if}
      {/if}

      <label for="convert-rate">Sample rate</label>
      {#if rates.length > 0}
        <select
          id="convert-rate"
          value={settings.maxSampleRate ?? ""}
          onchange={(event) =>
            (settings = { ...settings, maxSampleRate: optionalNumber(event.currentTarget.value) })}
        >
          <option value="">As the source</option>
          {#each rates as rate (rate)}
            <option value={rate}>At most {formatRate(rate)}</option>
          {/each}
        </select>
      {:else}
        <span class="note" id="convert-rate">Opus always plays at 48 kHz</span>
      {/if}

      {#if lossless}
        <label for="convert-depth">Bit depth</label>
        <select
          id="convert-depth"
          value={settings.maxBitDepth ?? ""}
          onchange={(event) =>
            (settings = { ...settings, maxBitDepth: optionalNumber(event.currentTarget.value) })}
        >
          <option value="">As the source</option>
          {#each BIT_DEPTHS as depth (depth)}
            <option value={depth}>At most {depth} bits</option>
          {/each}
        </select>
      {/if}

      <label for="convert-folder">Folder</label>
      <div class="pair">
        <input
          id="convert-folder"
          class="folder"
          bind:value={folder}
          placeholder="Where the converted files go"
          spellcheck="false"
        />
        <button type="button" onclick={chooseFolder}>Choose...</button>
      </div>

      <label for="convert-threads">Files at once</label>
      <select
        id="convert-threads"
        value={threads ?? ""}
        onchange={(event) => (threads = optionalNumber(event.currentTarget.value))}
      >
        <option value="">Automatic</option>
        {#each THREAD_CHOICES as count (count)}
          <option value={count}>{count}</option>
        {/each}
      </select>

      <span></span>
      <label class="check">
        <input type="checkbox" bind:checked={addToLibrary} disabled={!canAdd} />
        {#if canAdd}
          Add the converted files to the library
        {:else}
          Opus files cannot be added to the library, since Umak cannot play them
        {/if}
      </label>
    </div>

    <div class="plan" aria-live="polite">
      {#if !folder.trim()}
        <p class="note">
          Choose a folder for the converted files. Each file keeps its folders under its library
          folder.
        </p>
      {:else if planError}
        <p class="error">{planError}</p>
      {:else if plan}
        {#each describePlan(plan, settings.codec) as line, index (index)}
          <p>{line}</p>
        {/each}
        {#if plan.exampleOutput}
          <p class="note example" title={plan.exampleOutput}>For example: {plan.exampleOutput}</p>
        {/if}
      {:else if planning}
        <p class="note">Checking the files...</p>
      {/if}
    </div>

    <div class="buttons">
      <button type="button" onclick={() => dialog?.close()}>Cancel</button>
      <button class="primary" type="submit" disabled={!ready}>
        {actionLabel("convert.start")}
      </button>
    </div>
  </form>
</dialog>

<style>
  dialog {
    width: min(36em, calc(100vw - 2 * var(--space-md)));
    max-height: calc(100vh - 2 * var(--space-md));
    box-sizing: border-box;
    padding: var(--space-md);
    background: var(--color-surface);
    color: var(--color-text);
    border: 1px solid var(--color-border);
    border-radius: var(--radius);
    font-size: var(--font-size-body);
  }

  dialog::backdrop {
    background: var(--color-backdrop);
  }

  h2 {
    margin: 0 0 var(--space-md);
    font-size: var(--font-size-title);
    font-weight: 600;
  }

  .grid {
    display: grid;
    grid-template-columns: 8em minmax(0, 1fr);
    align-items: center;
    gap: var(--space-sm) var(--space-md);
  }

  label {
    color: var(--color-text-muted);
  }

  .check {
    display: flex;
    align-items: center;
    gap: var(--space-sm);
    color: var(--color-text);
  }

  .pair {
    display: flex;
    gap: var(--space-sm);
    min-width: 0;
  }

  .folder {
    flex: 1;
    min-width: 0;
  }

  .plan {
    margin: var(--space-md) 0;
    padding: var(--space-sm) var(--space-md);
    min-height: 3em;
    background: var(--color-bg);
    border-radius: var(--radius);
  }

  .plan p {
    margin: var(--space-xs) 0;
  }

  .note {
    color: var(--color-text-muted);
    font-size: var(--font-size-small);
  }

  .example {
    overflow-wrap: anywhere;
  }

  .error {
    color: var(--color-danger);
  }

  .buttons {
    display: flex;
    justify-content: flex-end;
    gap: var(--space-sm);
  }

  button,
  select,
  input:not([type="checkbox"]) {
    padding: var(--space-xs) var(--space-sm);
    background: var(--color-surface-raised);
    color: var(--color-text);
    border: 1px solid var(--color-border);
    border-radius: var(--radius);
    font: inherit;
  }

  input:not([type="checkbox"]) {
    background: var(--color-bg);
  }

  button {
    cursor: pointer;
    white-space: nowrap;
  }

  button:hover:not(:disabled) {
    background: var(--color-hover);
  }

  button.primary {
    background: var(--color-accent);
    border-color: var(--color-accent);
    color: var(--color-on-accent);
  }

  button:disabled {
    opacity: 0.5;
    cursor: default;
  }

  button:focus-visible,
  select:focus-visible,
  input:focus-visible {
    outline: 1px solid var(--color-accent);
    outline-offset: 1px;
  }
</style>
