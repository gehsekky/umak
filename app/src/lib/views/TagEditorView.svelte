<script lang="ts">
  import { actionLabel, runAction } from "../actions/registry";
  import { countOf } from "../lists/format";
  import VirtualList from "../lists/VirtualList.svelte";
  import { app } from "../state/app.svelte";
  import { tagDraft as draft } from "../tags/current.svelte";
  import { clearField, resetField, SHARED_FIELDS, valueProblem } from "../tags/plan";

  const tracks = $derived(draft.tracks);
  const unreadable = $derived(
    draft.allRead ? tracks.filter((track) => track.error !== null).length : 0,
  );
  const canPreview = $derived(
    draft.changes !== null && draft.problems.length === 0 && app.scan === null,
  );

  let start = $state(0);
  let end = $state(0);

  function fileName(path: string | null): string {
    return path?.split("/").pop() ?? "";
  }

  function typeInto(field: string, index: number, value: string): void {
    const state = draft.shared[field];
    state.values[index] = value;
    state.touched = true;
    state.cleared = false;
  }

  function addValue(field: string): void {
    const state = draft.shared[field];
    state.values.push("");
    state.touched = true;
    state.cleared = false;
  }

  function removeValue(field: string, index: number): void {
    const state = draft.shared[field];
    state.values.splice(index, 1);
    if (state.values.length === 0) state.values.push("");
    state.touched = true;
  }

  function placeholder(field: string): string {
    const state = draft.shared[field];
    if (state.cleared) return "Will be removed";
    if (state.mixed) return "Different values: type to replace them all";
    if (state.seen === 0 && !draft.allRead) return "Reading...";
    return "Empty";
  }
</script>

{#if draft.active}
  <div class="page">
    <header>
      <h1>{actionLabel("tags.edit")}</h1>
      <span class="meta">
        {countOf(tracks.length, "track")}
        {#if !draft.allRead && !draft.readError}
          · reading tags, {draft.loaded.toLocaleString()} of {tracks.length.toLocaleString()}
        {/if}
      </span>
    </header>

    {#if draft.readError}
      <p class="error">Could not read these tags: {draft.readError}</p>
    {/if}
    {#if unreadable > 0}
      <p class="error">{countOf(unreadable, "file")} could not be read. The preview shows why.</p>
    {/if}

    <div class="panes">
      <form class="shared" aria-label="Fields for all these tracks" onsubmit={(e) => e.preventDefault()}>
        <h2>{tracks.length === 1 ? "This track" : "All these tracks"}</h2>
        {#each SHARED_FIELDS as { field, label, list } (field)}
          {@const state = draft.shared[field]}
          {#if state}
            <div class="field" class:changed={state.touched} data-field={field}>
              <span class="label">{label}</span>
              <div class="values">
                {#each state.values as value, index (index)}
                  <div class="value">
                    <input
                      {value}
                      aria-label={state.values.length > 1 ? `${label} ${index + 1}` : label}
                      placeholder={placeholder(field)}
                      class:invalid={valueProblem(field, value) !== null}
                      oninput={(event) => typeInto(field, index, event.currentTarget.value)}
                    />
                    {#if state.values.length > 1}
                      <button
                        type="button"
                        class="small"
                        aria-label="Remove this {label.toLowerCase()}"
                        onclick={() => removeValue(field, index)}>×</button
                      >
                    {/if}
                  </div>
                {/each}
                {#if valueProblem(field, state.values[0] ?? "")}
                  <span class="problem">{valueProblem(field, state.values[0])}</span>
                {/if}
              </div>
              <div class="tools">
                {#if list}
                  <button
                    type="button"
                    class="small"
                    title="Add another {label.toLowerCase()}"
                    aria-label="Add another {label.toLowerCase()}"
                    onclick={() => addValue(field)}>+</button
                  >
                {/if}
                {#if state.mixed && !state.cleared}
                  <button type="button" class="small" onclick={() => clearField(state)}>Clear</button>
                {/if}
                {#if state.touched}
                  <button type="button" class="small" onclick={() => resetField(state)}>Reset</button>
                {/if}
              </div>
            </div>
          {/if}
        {/each}
      </form>

      <section class="own" aria-label="Fields for each track">
        <div class="own-heading">
          <h2>{tracks.length === 1 ? "Title and number" : "Each track"}</h2>
          {#if tracks.length > 1}
            <button
              type="button"
              disabled={!draft.allRead}
              title="Set track numbers from 1, in the order listed here"
              onclick={() => draft.numberTracks()}
            >
              Number in this order
            </button>
          {/if}
        </div>
        <div class="own-header">
          <span>Track</span>
          <span>Title</span>
          <span>File</span>
        </div>
        <VirtualList bind:start bind:end total={tracks.length} label="Tracks being edited">
          {#snippet row(index)}
            {@const track = tracks[index]}
            <div class="own-row" class:failed={track.error !== null} title={track.error ?? track.relPath ?? ""}>
              <input
                value={draft.shownValue(track, "trackNumber")}
                aria-label="Track number of {fileName(track.relPath)}"
                inputmode="numeric"
                disabled={!track.read || track.error !== null}
                class:invalid={valueProblem("trackNumber", draft.shownValue(track, "trackNumber")) !== null}
                oninput={(event) => draft.setOwn(track.id, "trackNumber", event.currentTarget.value)}
              />
              <input
                value={draft.shownValue(track, "title")}
                aria-label="Title of {fileName(track.relPath)}"
                placeholder={track.read ? "Empty" : "Reading..."}
                disabled={!track.read || track.error !== null}
                oninput={(event) => draft.setOwn(track.id, "title", event.currentTarget.value)}
              />
              <span class="file">{track.error ?? fileName(track.relPath)}</span>
            </div>
          {/snippet}
        </VirtualList>
      </section>
    </div>

    <footer>
      <button class="primary" disabled={!canPreview} onclick={() => runAction("tags.preview")}>
        {actionLabel("tags.preview")}
      </button>
      <button onclick={() => runAction("tags.closeDraft")}>{actionLabel("tags.closeDraft")}</button>
      <span class="meta note">
        {#if draft.problems.length > 0}
          <span class="problem">{draft.problems[0]}</span>
        {:else if app.scan}
          Waiting for the current job to finish.
        {:else if draft.changes === null}
          Nothing has changed yet. Fields you leave alone are not written.
        {:else}
          Nothing is written until you have seen the preview.
        {/if}
      </span>
    </footer>
  </div>
{:else}
  <p class="empty">Select tracks in the library, then choose {actionLabel("tags.edit")}.</p>
{/if}

<style>
  .page {
    display: flex;
    flex-direction: column;
    flex: 1;
    min-height: 0;
  }

  header {
    display: flex;
    align-items: baseline;
    gap: var(--space-sm);
    padding: var(--space-md) var(--space-md) var(--space-sm);
  }

  h1 {
    margin: 0;
    font-size: var(--font-size-title);
    font-weight: 600;
  }

  h2 {
    margin: 0 0 var(--space-sm);
    font-size: var(--font-size-body);
    font-weight: 600;
  }

  .meta,
  .empty {
    color: var(--color-text-muted);
    font-size: var(--font-size-small);
  }

  .empty {
    padding: var(--space-lg);
  }

  .error,
  .problem {
    color: var(--color-danger);
  }

  .error {
    margin: 0;
    padding: 0 var(--space-md) var(--space-sm);
  }

  .problem {
    font-size: var(--font-size-small);
  }

  /* Side by side when they fit. Otherwise one above the other, and this area scrolls. */
  .panes {
    display: flex;
    flex-wrap: wrap;
    gap: var(--space-md);
    flex: 1;
    min-height: 0;
    padding: 0 var(--space-md) var(--space-md);
    overflow-y: auto;
  }

  .shared {
    flex: 1 1 22em;
    min-width: 0;
  }

  .own {
    display: flex;
    flex-direction: column;
    flex: 1.3 1 26em;
    min-width: 0;
    min-height: 18em;
    height: 100%;
  }

  .field {
    display: grid;
    grid-template-columns: 7em minmax(0, 1fr) auto;
    align-items: start;
    gap: var(--space-sm);
    margin-bottom: var(--space-xs);
  }

  .label {
    padding-top: var(--space-xs);
    color: var(--color-text-muted);
  }

  .field.changed .label {
    color: var(--color-accent);
  }

  .values,
  .value {
    display: flex;
    min-width: 0;
    gap: var(--space-xs);
  }

  .values {
    flex-direction: column;
  }

  .tools {
    display: flex;
    gap: var(--space-xs);
    /* Keeps the boxes lined up whichever buttons a field has at the moment. */
    min-width: 6em;
  }

  input {
    flex: 1;
    width: 100%;
    min-width: 0;
    box-sizing: border-box;
    padding: var(--space-xs) var(--space-sm);
    background: var(--color-surface);
    color: var(--color-text);
    border: 1px solid var(--color-border);
    border-radius: var(--radius);
    font: inherit;
    outline: none;
  }

  input:focus {
    border-color: var(--color-accent);
  }

  input.invalid {
    border-color: var(--color-danger);
  }

  input:disabled {
    color: var(--color-text-muted);
  }

  .field.changed input {
    border-color: var(--color-accent);
  }

  .own-heading {
    display: flex;
    align-items: baseline;
    justify-content: space-between;
    gap: var(--space-sm);
  }

  .own-header,
  .own-row {
    display: grid;
    grid-template-columns: 4em minmax(0, 2fr) minmax(0, 1fr);
    align-items: center;
    gap: var(--space-xs);
  }

  .own-header {
    padding-bottom: var(--space-xs);
    color: var(--color-text-muted);
    border-bottom: 1px solid var(--color-border);
  }

  /* In line with the text in the boxes below. */
  .own-header span {
    padding-left: var(--space-sm);
    font-size: var(--font-size-small);
  }

  .own-header span:last-child {
    padding-left: 0;
  }

  .own-row {
    height: var(--size-row);
    box-sizing: border-box;
  }

  .own-row input {
    padding-block: 0;
    height: calc(var(--size-row) - 4px);
    background: transparent;
    border-color: transparent;
  }

  .own-row input:hover:not(:disabled),
  .own-row input:focus {
    background: var(--color-surface);
    border-color: var(--color-border);
  }

  .own-row input:focus {
    border-color: var(--color-accent);
  }

  .own-row input.invalid {
    border-color: var(--color-danger);
  }

  .file {
    overflow: hidden;
    white-space: nowrap;
    text-overflow: ellipsis;
    color: var(--color-text-muted);
    font-size: var(--font-size-small);
  }

  .own-row.failed .file {
    color: var(--color-danger);
  }

  footer {
    display: flex;
    align-items: center;
    gap: var(--space-sm);
    padding: var(--space-sm) var(--space-md);
    border-top: 1px solid var(--color-border);
  }

  .note {
    min-width: 0;
    overflow: hidden;
    white-space: nowrap;
    text-overflow: ellipsis;
  }

  button {
    flex: none;
    padding: var(--space-xs) var(--space-sm);
    background: var(--color-surface-raised);
    color: var(--color-text);
    border: 1px solid var(--color-border);
    border-radius: var(--radius);
    font: inherit;
    font-size: var(--font-size-small);
    white-space: nowrap;
    cursor: pointer;
  }

  button.small {
    padding: var(--space-xs);
    min-width: 2em;
  }

  button:hover:not(:disabled) {
    background: var(--color-hover);
  }

  button:disabled {
    opacity: 0.5;
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
</style>
