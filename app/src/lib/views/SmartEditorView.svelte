<script lang="ts">
  // Creates or changes a smart playlist. The list below follows the rule as it is edited, so
  // what it will hold is plain before anything is saved.
  import { untrack } from "svelte";
  import { runAction } from "../actions/registry";
  import { call } from "../api/client";
  import type { PlaylistItem } from "../api/generated/PlaylistItem";
  import type { SmartField } from "../api/generated/SmartField";
  import type { SmartOp } from "../api/generated/SmartOp";
  import type { SmartRule } from "../api/generated/SmartRule";
  import type { SmartSort } from "../api/generated/SmartSort";
  import { countOf, formatTotalTime } from "../lists/format";
  import { PagedList, type Source } from "../lists/pagedList.svelte";
  import { Selection } from "../lists/selection.svelte";
  import TrackTable from "../lists/TrackTable.svelte";
  import {
    DAY_UNITS,
    defaultCondition,
    FIELDS,
    kindOf,
    newRule,
    opsFor,
    RATING_CHOICES,
    ruleProblem,
    SORTS,
    splitDays,
    unitOf,
    withOp,
  } from "../smart/rule";
  import { app } from "../state/app.svelte";

  /** Waits for typing to pause before asking the core again. */
  const PREVIEW_DELAY_MS = 200;

  const id = $derived(app.view.kind === "smartEditor" ? app.view.id : null);
  const existing = untrack(() => (id ? app.playlist(id) : undefined));

  let name = $state(existing?.name ?? "");
  // Editing a copy, so leaving without saving changes nothing.
  let rule = $state<SmartRule>(existing?.rule ? structuredClone(existing.rule) : newRule());
  /** Each date condition's chosen unit, by its place in the list. */
  let dayUnits = $state<number[]>(rule.conditions.map((one) => splitDays(one.number).unit));
  let formats = $state.raw<string[]>([]);
  let total = $state<number | null>(null);
  let durationMs = $state(0);
  let saving = $state(false);

  const problem = $derived(ruleProblem(rule));
  const canSave = $derived(problem === null && name.trim() !== "" && !saving);

  function source(current: SmartRule): Source<PlaylistItem> {
    return async (offset, limit) => {
      const page = await call("playlists.smart.preview", { rule: current, offset, limit });
      total = page.total;
      durationMs = page.durationMs;
      return { rows: page.items, total: page.total };
    };
  }

  const list = new PagedList<PlaylistItem>(
    source(untrack(() => $state.snapshot(rule))),
    (item) => item.track.id,
  );
  const selection = new Selection();

  call("library.formats", {}).then(
    (found) => (formats = found.formats),
    () => {},
  );

  $effect(() => {
    const current = $state.snapshot(rule);
    void app.libraryVersion;
    if (ruleProblem(current)) return;
    const waiting = setTimeout(() => {
      list.setSource(source(current));
      selection.clear();
    }, PREVIEW_DELAY_MS);
    return () => clearTimeout(waiting);
  });

  function setField(index: number, field: SmartField): void {
    rule.conditions[index] = defaultCondition(field);
    dayUnits[index] = splitDays(rule.conditions[index].number).unit;
  }

  function setOp(index: number, op: SmartOp): void {
    rule.conditions[index] = withOp(rule.conditions[index], op);
    dayUnits[index] = splitDays(rule.conditions[index].number).unit;
  }

  function addCondition(): void {
    rule.conditions.push(defaultCondition("artist"));
    dayUnits.push(1);
  }

  function removeCondition(index: number): void {
    rule.conditions.splice(index, 1);
    dayUnits.splice(index, 1);
  }

  /** A box left empty is no number, which the rule then reports as a problem. */
  function parsed(text: string): number | null {
    return text.trim() === "" ? null : Number(text);
  }

  function setDays(index: number, amount: number | null, unit: number): void {
    dayUnits[index] = unit;
    rule.conditions[index].number = amount === null ? null : amount * unit;
  }

  async function save(event: SubmitEvent): Promise<void> {
    event.preventDefault();
    if (!canSave) return;
    saving = true;
    await runAction("playlists.saveSmart", { id, name: name.trim(), rule: $state.snapshot(rule) });
    saving = false;
  }
</script>

<div class="page">
  <form onsubmit={save} aria-label="Smart playlist">
    <div class="line">
      <input
        class="name"
        bind:value={name}
        placeholder="Playlist name"
        aria-label="Playlist name"
      />
      <span class="spacer"></span>
      <button class="primary" type="submit" disabled={!canSave}>Save</button>
      <button type="button" onclick={() => runAction("playlists.leaveSmart", { id })}>Cancel</button>
    </div>

    <div class="line">
      <span>Tracks must match</span>
      <select
        aria-label="How many rules must match"
        value={rule.matchAll ? "all" : "any"}
        onchange={(event) => (rule.matchAll = event.currentTarget.value === "all")}
      >
        <option value="all">all</option>
        <option value="any">any</option>
      </select>
      <span>of these rules</span>
    </div>

    <div class="conditions">
      {#each rule.conditions as condition, index (index)}
        {@const kind = kindOf(condition.field)}
        <div class="line" data-testid="smart-condition">
          <select
            aria-label="Field"
            value={condition.field}
            onchange={(event) => setField(index, event.currentTarget.value as SmartField)}
          >
            {#each FIELDS as one (one.field)}
              <option value={one.field}>{one.label}</option>
            {/each}
          </select>
          <select
            aria-label="Comparison"
            value={condition.op}
            onchange={(event) => setOp(index, event.currentTarget.value as SmartOp)}
          >
            {#each opsFor(condition.field) as one (one.op)}
              <option value={one.op}>{one.label}</option>
            {/each}
          </select>
          {#if condition.op !== "empty"}
            {#if kind === "text"}
              <input
                aria-label="Text"
                value={condition.text ?? ""}
                oninput={(event) => (condition.text = event.currentTarget.value)}
              />
            {:else if kind === "format"}
              <select
                aria-label="Format"
                value={condition.text ?? ""}
                onchange={(event) => (condition.text = event.currentTarget.value)}
              >
                <option value="" disabled>Choose a format</option>
                {#each formats as format (format)}
                  <option value={format}>{format.toUpperCase()}</option>
                {/each}
              </select>
            {:else if kind === "rating"}
              <select
                aria-label="Rating"
                value={condition.number}
                onchange={(event) => (condition.number = Number(event.currentTarget.value))}
              >
                {#each RATING_CHOICES as one (one.value)}
                  <option value={one.value}>{one.label}</option>
                {/each}
              </select>
            {:else if kind === "date"}
              {@const unit = dayUnits[index] ?? 1}
              <input
                class="number"
                type="number"
                min="1"
                aria-label="How long"
                value={condition.number === null ? "" : condition.number / unit}
                oninput={(event) => setDays(index, parsed(event.currentTarget.value), unit)}
              />
              <select
                aria-label="Unit of time"
                value={unit}
                onchange={(event) => {
                  const amount = condition.number === null ? null : condition.number / unit;
                  setDays(index, amount, Number(event.currentTarget.value));
                }}
              >
                {#each DAY_UNITS as one (one.days)}
                  <option value={one.days}>{one.label}</option>
                {/each}
              </select>
            {:else}
              <input
                class="number"
                type="number"
                min="0"
                aria-label="Number"
                value={condition.number ?? ""}
                oninput={(event) => (condition.number = parsed(event.currentTarget.value))}
              />
              {#if unitOf(condition.field)}<span class="meta">{unitOf(condition.field)}</span>{/if}
            {/if}
          {/if}
          <button type="button" aria-label="Remove this rule" onclick={() => removeCondition(index)}>
            ×
          </button>
        </div>
      {/each}
    </div>

    <div class="line">
      <button type="button" onclick={addCondition}>Add a rule</button>
      <span class="spacer"></span>
      <label>
        Sort by
        <select
          value={rule.sort}
          onchange={(event) => (rule.sort = event.currentTarget.value as SmartSort)}
        >
          {#each SORTS as one (one.sort)}
            <option value={one.sort}>{one.label}</option>
          {/each}
        </select>
      </label>
      <select
        aria-label="Sort direction"
        value={rule.descending ? "descending" : "ascending"}
        onchange={(event) => (rule.descending = event.currentTarget.value === "descending")}
      >
        <option value="ascending">Ascending</option>
        <option value="descending">Descending</option>
      </select>
      <label>
        <input
          type="checkbox"
          checked={rule.limit !== null}
          onchange={(event) => (rule.limit = event.currentTarget.checked ? 50 : null)}
        />
        Keep only
      </label>
      {#if rule.limit !== null}
        <input
          class="number"
          type="number"
          min="1"
          aria-label="How many tracks to keep"
          value={rule.limit}
          oninput={(event) => (rule.limit = parsed(event.currentTarget.value) ?? 0)}
        />
        <span>tracks</span>
      {/if}
    </div>

    <p class="meta result" data-testid="smart-total">
      {#if problem}
        <span class="problem">{problem}</span>
      {:else if total !== null}
        {countOf(total, "track")} match right now, {formatTotalTime(durationMs)}.
      {/if}
    </p>
  </form>

  <TrackTable
    {list}
    {selection}
    label="Tracks this rule gives"
    track={(item) => item.track}
    number={(_, index) => String(index + 1)}
    onactivate={(item) => runAction("player.playTracks", { trackIds: [item.track.id], startIndex: 0 })}
  >
    {#snippet empty()}
      <p>No tracks match this rule right now.</p>
    {/snippet}
  </TrackTable>
</div>

<style>
  .page {
    display: flex;
    flex-direction: column;
    flex: 1;
    min-height: 0;
  }

  form {
    display: flex;
    flex-direction: column;
    gap: var(--space-sm);
    padding: var(--space-md);
    max-height: 55%;
    overflow-y: auto;
    border-bottom: 1px solid var(--color-border);
  }

  .conditions {
    display: flex;
    flex-direction: column;
    gap: var(--space-xs);
  }

  .line {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: var(--space-xs) var(--space-sm);
    min-width: 0;
  }

  label {
    display: flex;
    align-items: center;
    gap: var(--space-xs);
  }

  .spacer {
    flex: 1;
  }

  .meta {
    color: var(--color-text-muted);
    font-size: var(--font-size-small);
  }

  .result {
    margin: 0;
    min-height: 1.4em;
  }

  .problem {
    color: var(--color-danger);
  }

  input:not([type="checkbox"]),
  select {
    box-sizing: border-box;
    min-width: 0;
    padding: var(--space-xs) var(--space-sm);
    background: var(--color-surface);
    color: var(--color-text);
    border: 1px solid var(--color-border);
    border-radius: var(--radius);
    font: inherit;
    outline: none;
  }

  input:focus,
  select:focus {
    border-color: var(--color-accent);
  }

  input:not([type="checkbox"]) {
    flex: 1 1 12em;
    max-width: 24em;
  }

  input.number {
    flex: 0 0 6em;
    width: 6em;
  }

  input.name {
    flex: 1 1 16em;
    font-size: var(--font-size-title);
    font-weight: 600;
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
