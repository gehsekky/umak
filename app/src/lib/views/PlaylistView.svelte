<script lang="ts">
  import { untrack } from "svelte";
  import { actionLabel, runAction } from "../actions/registry";
  import { call } from "../api/client";
  import type { PlaylistItem } from "../api/generated/PlaylistItem";
  import { activateTrack } from "../lists/activate";
  import { countOf, formatTotalTime } from "../lists/format";
  import { PagedList, type Source } from "../lists/pagedList.svelte";
  import { planStep } from "../lists/reorder";
  import { Selection } from "../lists/selection.svelte";
  import TrackTable from "../lists/TrackTable.svelte";
  import { app } from "../state/app.svelte";

  const id = $derived(app.view.kind === "playlist" ? app.view.id : "");
  const playlist = $derived(app.playlist(id));

  function source(playlistId: string): Source<PlaylistItem> {
    return async (offset, limit) => {
      const page = await call("playlists.items", { id: playlistId, offset, limit });
      return { rows: page.items, total: page.total };
    };
  }

  // A smart playlist's order changes as tracks come and go, so its rows are held by track. It
  // has no track twice, and no entries of its own to hold by position.
  const list = new PagedList<PlaylistItem>(
    source(untrack(() => id)),
    (item) => (app.playlist(id)?.smart ? item.track.id : item.position),
  );
  const selection = new Selection();
  let table = $state<TrackTable<PlaylistItem>>();
  let renaming = $state(false);
  let nameInput = $state<HTMLInputElement>();
  let draftName = $state("");
  let confirmingDelete = $state(false);
  /** A smart playlist's totals, which the playlist list leaves to this page to ask for. */
  let smartTotals = $state<{ total: number; durationMs: number } | null>(null);

  const smart = $derived(playlist?.smart ?? false);
  const trackCount = $derived(smart ? smartTotals?.total : playlist?.trackCount);
  const durationMs = $derived(smart ? smartTotals?.durationMs : playlist?.durationMs);

  $effect(() => {
    const rule = playlist?.rule;
    void app.libraryVersion;
    if (!smart || !rule) {
      smartTotals = null;
      return;
    }
    let current = true;
    call("playlists.smart.preview", { rule: $state.snapshot(rule), offset: 0, limit: 0 }).then(
      (page) => {
        if (current) smartTotals = { total: page.total, durationMs: page.durationMs };
      },
      () => {},
    );
    return () => {
      current = false;
    };
  });

  let shownId = untrack(() => id);
  $effect(() => {
    if (id !== shownId) {
      shownId = id;
      renaming = false;
      confirmingDelete = false;
      list.setSource(source(id));
      selection.clear();
      table?.scrollToTop();
    }
  });

  $effect(() => {
    // Both can change what a row shows; a scan may also rewrite track details.
    void app.playlistsVersion;
    void app.libraryVersion;
    untrack(() => {
      if (list.ready) void list.reload();
    });
  });

  $effect(() => {
    nameInput?.focus();
    nameInput?.select();
  });

  function startRename(): void {
    draftName = playlist?.name ?? "";
    renaming = true;
  }

  async function finishRename(event: SubmitEvent): Promise<void> {
    event.preventDefault();
    const name = draftName.trim();
    if (name && name !== playlist?.name) {
      await runAction("playlists.rename", { id, name });
    }
    renaming = false;
  }

  /** Moves the selected rows up or down by one, keeping them together and selected. */
  async function move(direction: -1 | 1): Promise<void> {
    const all = (await list.keysInRange(0, list.total)) as number[];
    const plan = planStep(all, (position) => selection.has(position), direction);
    if (!plan) return;
    const { positions, before, newStart } = plan;
    if (!(await runAction("playlists.moveItems", { id, positions, before }))) return;

    await list.reload();
    const keys = await list.keysInRange(newStart, newStart + positions.length);
    selection.setRange(keys, newStart);
    selection.anchor = newStart;
  }

  /** The selected rows' tracks, in list order. Entries are selected by position. */
  async function selectedTrackIds(): Promise<string[]> {
    const rows = await list.rowsInRange(0, list.total);
    return rows.filter((item) => selection.has(list.key(item))).map((item) => item.track.id);
  }

  async function removeSelected(): Promise<void> {
    const positions = [...selection.keys] as number[];
    if (await runAction("playlists.removeItems", { id, positions })) {
      selection.clear();
    }
  }
</script>

{#if playlist}
  <div class="header">
    {#if renaming}
      <form onsubmit={finishRename}>
        <input
          bind:this={nameInput}
          bind:value={draftName}
          aria-label="Playlist name"
          onblur={() => (renaming = false)}
          onkeydown={(event) => {
            if (event.key === "Escape") renaming = false;
          }}
        />
      </form>
    {:else}
      <h1>{playlist.name}</h1>
    {/if}
    <span class="meta">
      {#if trackCount !== undefined && trackCount !== null}
        {countOf(trackCount, "track")}, {formatTotalTime(durationMs ?? 0)}
      {/if}
      {#if smart}· smart playlist{/if}
    </span>
    <span class="spacer"></span>
    <button
      class="primary"
      disabled={!trackCount}
      onclick={() => runAction("player.playPlaylist", { id, startIndex: 0 })}
    >
      {actionLabel("player.playPlaylist")}
    </button>
    {#if smart}
      <button onclick={() => runAction("nav.smartEditor", { id })}>Edit rule</button>
    {:else}
      <button onclick={startRename}>Rename</button>
    {/if}
    {#if confirmingDelete}
      <button class="danger" onclick={() => runAction("playlists.delete", { id })}>
        Confirm delete
      </button>
      <button onclick={() => (confirmingDelete = false)}>Keep</button>
    {:else}
      <button onclick={() => (confirmingDelete = true)}>{actionLabel("playlists.delete")}</button>
    {/if}
  </div>

  <div class="toolbar">
    {#if selection.size > 0}
      <span class="meta">{countOf(selection.size, "track")} selected</span>
      <button
        onclick={async () => runAction("queue.playNext", { trackIds: await selectedTrackIds() })}
      >
        {actionLabel("queue.playNext")}
      </button>
      <button onclick={async () => runAction("queue.add", { trackIds: await selectedTrackIds() })}>
        {actionLabel("queue.add")}
      </button>
      <button onclick={async () => runAction("tags.edit", { trackIds: await selectedTrackIds() })}>
        {actionLabel("tags.edit")}
      </button>
      <button onclick={async () => runAction("convert.open", { trackIds: await selectedTrackIds() })}>
        {actionLabel("convert.open")}
      </button>
      {#if !smart}
        <button onclick={() => move(-1)}>Move up</button>
        <button onclick={() => move(1)}>Move down</button>
        <button onclick={removeSelected}>{actionLabel("playlists.removeItems")}</button>
      {/if}
    {:else if smart}
      <span class="meta">This list follows its rule, so it changes with your library.</span>
    {:else}
      <span class="meta">Select tracks to reorder or remove them.</span>
    {/if}
  </div>

  <TrackTable
    bind:this={table}
    {list}
    {selection}
    label="Tracks in {playlist.name}"
    track={(item) => item.track}
    number={(_, index) => String(index + 1)}
    onactivate={(item, index) =>
      activateTrack(
        item.track.id,
        () => (selection.has(list.key(item)) ? selectedTrackIds() : []),
        () => runAction("player.playPlaylist", { id, startIndex: index }),
      )}
  >
    {#snippet empty()}
      {#if smart}
        <p>No tracks match this rule right now.</p>
      {:else}
        <p>This playlist is empty. Select tracks in the library to add them.</p>
      {/if}
    {/snippet}
  </TrackTable>
{:else}
  <p class="gone">This playlist no longer exists.</p>
{/if}

<style>
  .header {
    display: flex;
    align-items: center;
    gap: var(--space-sm);
    padding: var(--space-md) var(--space-md) var(--space-sm);
  }

  h1 {
    margin: 0;
    font-size: var(--font-size-title);
    font-weight: 600;
    overflow: hidden;
    white-space: nowrap;
    text-overflow: ellipsis;
  }

  input {
    padding: var(--space-xs) var(--space-sm);
    background: var(--color-surface);
    color: var(--color-text);
    border: 1px solid var(--color-accent);
    border-radius: var(--radius);
    font: inherit;
    font-size: var(--font-size-title);
    outline: none;
  }

  .meta {
    color: var(--color-text-muted);
    font-size: var(--font-size-small);
    white-space: nowrap;
  }

  .spacer {
    flex: 1;
  }

  .toolbar {
    flex-wrap: wrap;
    display: flex;
    align-items: center;
    gap: var(--space-sm);
    min-height: var(--size-row);
    padding: var(--space-xs) var(--space-md);
    border-bottom: 1px solid var(--color-border);
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

  button:hover {
    background: var(--color-hover);
  }

  button:focus-visible {
    outline: 1px solid var(--color-accent);
    outline-offset: 1px;
  }

  button.primary:enabled {
    background: var(--color-accent);
    border-color: var(--color-accent);
    color: var(--color-on-accent);
  }

  button:disabled {
    color: var(--color-text-muted);
    cursor: default;
  }

  button.danger {
    border-color: var(--color-danger);
    color: var(--color-danger);
  }

  .gone {
    padding: var(--space-lg);
    color: var(--color-text-muted);
  }
</style>
