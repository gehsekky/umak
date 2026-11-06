<script lang="ts">
  // The player playlist. Everything that plays is in this one list.
  import { untrack } from "svelte";
  import { actionLabel, runAction } from "../actions/registry";
  import type { PlaylistItem } from "../api/generated/PlaylistItem";
  import { countOf } from "../lists/format";
  import { PagedList, type Source } from "../lists/pagedList.svelte";
  import { planDrop, planStep, type MovePlan } from "../lists/reorder";
  import { Selection } from "../lists/selection.svelte";
  import TrackTable from "../lists/TrackTable.svelte";
  import { playerClient } from "../player/client";
  import { player } from "../player/state.svelte";
  import { app } from "../state/app.svelte";

  const source: Source<PlaylistItem> = async (offset, limit) => {
    const page = await playerClient.queueItems(offset, limit);
    return { rows: page.items, total: page.total };
  };

  const list = new PagedList<PlaylistItem>(source, (item) => item.position);
  const selection = new Selection();
  let table = $state<TrackTable<PlaylistItem>>();
  let saving = $state(false);
  let saveName = $state("");
  let saveInput = $state<HTMLInputElement>();
  let confirmingClear = $state(false);

  $effect(() => {
    void player.queueVersion;
    void app.libraryVersion;
    untrack(() => {
      if (list.ready) void list.reload();
    });
  });

  $effect(() => {
    // Positions in a new list say nothing about the old selection.
    if (player.queueReplacements > 0) untrack(() => selection.clear());
  });

  $effect(() => {
    saveInput?.focus();
  });

  const currentIndex = $derived(
    player.status && player.status.state !== "stopped" ? player.status.index : null,
  );

  // Follow playback, unless the user is working with rows.
  $effect(() => {
    const index = currentIndex;
    untrack(() => {
      if (index !== null && selection.size === 0) table?.reveal(index);
    });
  });

  async function applyMove(plan: MovePlan | null): Promise<void> {
    if (!plan) return;
    const { positions, before, newStart } = plan;
    if (!(await runAction("queue.move", { positions, before }))) return;
    await list.reload();
    const keys = await list.keysInRange(newStart, newStart + positions.length);
    selection.setRange(keys, newStart);
    selection.anchor = newStart;
    table?.reveal(newStart);
  }

  async function allPositions(): Promise<number[]> {
    return (await list.keysInRange(0, list.total)) as number[];
  }

  async function drop(gap: number): Promise<void> {
    const all = await allPositions();
    await applyMove(planDrop(all, (position) => selection.has(position), gap));
  }

  async function step(direction: -1 | 1): Promise<void> {
    const all = await allPositions();
    await applyMove(planStep(all, (position) => selection.has(position), direction));
  }

  async function removeSelected(): Promise<void> {
    const positions = [...selection.keys] as number[];
    if (await runAction("queue.remove", { positions })) selection.clear();
  }

  /** Keys beyond the table's own. Returns whether it used the key. */
  function keys(event: KeyboardEvent): boolean {
    if (event.key === "Delete" && selection.size > 0) {
      void removeSelected();
      return true;
    }
    if (event.altKey && (event.key === "ArrowUp" || event.key === "ArrowDown")) {
      void step(event.key === "ArrowUp" ? -1 : 1);
      return true;
    }
    return false;
  }

  async function finishSave(event: SubmitEvent): Promise<void> {
    event.preventDefault();
    const name = saveName.trim();
    if (name && (await runAction("queue.save", { name }))) {
      saving = false;
      saveName = "";
    }
  }

  async function clearAll(): Promise<void> {
    confirmingClear = false;
    if (await runAction("queue.clear")) selection.clear();
  }
</script>

<div class="header">
  <h2>Queue</h2>
  <span class="meta">{list.ready ? countOf(list.total, "track") : ""}</span>
  <span class="spacer"></span>
  {#if confirmingClear}
    <button class="danger" onclick={clearAll}>Confirm clear</button>
    <button onclick={() => (confirmingClear = false)}>Keep</button>
  {:else}
    <button disabled={list.total === 0} onclick={() => (saving = !saving)}>Save</button>
    <button disabled={list.total === 0} onclick={() => (confirmingClear = true)}>
      {actionLabel("queue.clear")}
    </button>
  {/if}
</div>

{#if saving}
  <form class="save" onsubmit={finishSave}>
    <input
      bind:this={saveInput}
      bind:value={saveName}
      placeholder="Playlist name"
      aria-label="Name for the saved playlist"
      onkeydown={(event) => {
        if (event.key === "Escape") saving = false;
      }}
    />
    <button class="primary" type="submit" disabled={!saveName.trim()}>
      {actionLabel("queue.save")}
    </button>
  </form>
{/if}

<!-- Always here, so selecting a row cannot shift the list under the pointer. -->
<div class="selected">
  {#if selection.size > 0}
    <span class="meta">{countOf(selection.size, "track")} selected</span>
    <span class="spacer"></span>
    <button onclick={removeSelected}>{actionLabel("queue.remove")}</button>
  {:else}
    <span class="meta">Drag to reorder. Delete removes.</span>
  {/if}
</div>

<TrackTable
  bind:this={table}
  {list}
  {selection}
  compact
  label="Queue"
  track={(item) => item.track}
  number={(_, index) => String(index + 1)}
  isCurrent={(_, index) => index === currentIndex}
  onactivate={(_, index) => runAction("queue.play", { index })}
  onkeys={keys}
  onreorder={drop}
>
  {#snippet empty()}
    <p>Nothing queued. Select tracks in the library to add them.</p>
  {/snippet}
</TrackTable>

<style>
  .header,
  .save,
  .selected {
    display: flex;
    align-items: center;
    gap: var(--space-xs);
    padding: var(--space-xs) var(--space-sm);
    border-bottom: 1px solid var(--color-border);
  }

  .selected {
    /* The height with its button, so the list stays put with or without one. */
    min-height: calc(var(--size-row) + var(--space-xs));
    box-sizing: border-box;
  }

  h2 {
    margin: 0;
    font-size: var(--font-size-body);
    font-weight: 600;
  }

  .meta {
    color: var(--color-text-muted);
    font-size: var(--font-size-small);
    white-space: nowrap;
  }

  .spacer {
    flex: 1;
  }

  input {
    flex: 1;
    min-width: 0;
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

  button {
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

  button.danger {
    border-color: var(--color-danger);
    color: var(--color-danger);
  }

  button:focus-visible {
    outline: 1px solid var(--color-accent);
    outline-offset: 1px;
  }
</style>
