<script lang="ts">
  import { untrack } from "svelte";
  import { actionLabel, runAction } from "../actions/registry";
  import { call } from "../api/client";
  import type { TrackRow } from "../api/generated/TrackRow";
  import type { TrackSort } from "../api/generated/TrackSort";
  import { countOf } from "../lists/format";
  import { PagedList, type Source } from "../lists/pagedList.svelte";
  import { Selection } from "../lists/selection.svelte";
  import TrackTable, { type SortState } from "../lists/TrackTable.svelte";
  import { app } from "../state/app.svelte";

  let query = $state("");
  const INITIAL_SORT: SortState = { sort: "album", descending: false };
  let sort = $state<SortState>(INITIAL_SORT);
  let searchBox = $state<HTMLInputElement>();
  let table = $state<TrackTable<TrackRow>>();
  let addTarget = $state("");

  function source(text: string, order: SortState): Source<TrackRow> {
    return async (offset, limit) => {
      const page = await call("library.tracks.page", {
        query: text.trim() || null,
        sort: order.sort,
        descending: order.descending,
        offset,
        limit,
      });
      return { rows: page.tracks, total: page.total };
    };
  }

  const list = new PagedList<TrackRow>(source("", INITIAL_SORT), (row) => row.id);
  const selection = new Selection();

  function showContents(): void {
    list.setSource(source(query, sort));
    selection.clear();
    table?.scrollToTop();
  }

  function choose(next: TrackSort): void {
    sort = {
      sort: next,
      descending: sort.sort === next ? !sort.descending : false,
    };
    showContents();
  }

  function toggleRecent(): void {
    sort =
      sort.sort === "added"
        ? { sort: "album", descending: false }
        : { sort: "added", descending: true };
    showContents();
  }

  $effect(() => {
    void app.libraryVersion;
    untrack(() => {
      if (list.ready) void list.reload();
    });
  });

  $effect(() => {
    if (app.searchFocusRequests > 0) {
      searchBox?.focus();
      searchBox?.select();
    }
  });

  /** Plays the selection if the row is part of it, and otherwise just that song. */
  function activate(row: TrackRow): void {
    const trackIds =
      selection.size > 1 && selection.has(row.id) ? ([...selection.keys] as string[]) : [row.id];
    const startIndex = Math.max(trackIds.indexOf(row.id), 0);
    void runAction("player.playTracks", { trackIds, startIndex });
  }

  function playSelection(): void {
    void runAction("player.playTracks", {
      trackIds: [...selection.keys] as string[],
      startIndex: 0,
    });
  }

  async function addSelection(): Promise<void> {
    if (!addTarget) return;
    const trackIds = [...selection.keys] as string[];
    await runAction("playlists.addTracks", { id: addTarget, trackIds });
    addTarget = "";
  }
</script>

<div class="toolbar">
  <input
    bind:this={searchBox}
    bind:value={query}
    class="search"
    type="search"
    placeholder="Search title, artist, album, genre (Ctrl+F)"
    aria-label={actionLabel("library.focusSearch")}
    oninput={showContents}
    onkeydown={(event) => {
      if (event.key === "Escape" && query) {
        query = "";
        showContents();
      } else if (event.key === "ArrowDown") {
        event.preventDefault();
        table?.focus();
      }
    }}
  />
  <button class:on={sort.sort === "added"} onclick={toggleRecent}>Recently added</button>
  <span class="count">
    {#if selection.size > 0}
      {countOf(selection.size, "track")} selected
    {:else if list.ready}
      {countOf(list.total, "track")}
    {/if}
  </span>
  {#if selection.size > 0}
    <button class="primary" onclick={playSelection}>{actionLabel("player.playTracks")}</button>
    <select
      bind:value={addTarget}
      onchange={addSelection}
      aria-label={actionLabel("playlists.addTracks")}
      disabled={app.playlists.length === 0}
    >
      <option value="">
        {app.playlists.length === 0 ? "Create a playlist first" : actionLabel("playlists.addTracks")}
      </option>
      {#each app.playlists as playlist (playlist.id)}
        <option value={playlist.id}>{playlist.name}</option>
      {/each}
    </select>
  {/if}
</div>

<TrackTable
  bind:this={table}
  {list}
  {selection}
  label="Library tracks"
  track={(row) => row}
  number={(row) => (row.trackNo > 0 ? String(row.trackNo) : "")}
  {sort}
  onsort={choose}
  onactivate={activate}
>
  {#snippet empty()}
    {#if query.trim()}
      <p>No tracks match "{query.trim()}"</p>
    {:else if app.roots.length === 0}
      <p>Add a folder to start your library.</p>
      <button class="primary" onclick={() => runAction("library.addFolder")}>
        {actionLabel("library.addFolder")}
      </button>
    {:else if app.scan}
      <p>Scanning your folders...</p>
    {:else}
      <p>No music found in your folders.</p>
    {/if}
  {/snippet}
</TrackTable>

<style>
  .toolbar {
    display: flex;
    align-items: center;
    gap: var(--space-sm);
    padding: var(--space-sm) var(--space-md);
    border-bottom: 1px solid var(--color-border);
  }

  .search {
    flex: 1;
    min-width: 0;
    max-width: 32em;
    padding: var(--space-xs) var(--space-sm);
    background: var(--color-surface);
    color: var(--color-text);
    border: 1px solid var(--color-border);
    border-radius: var(--radius);
    font: inherit;
    outline: none;
  }

  .search:focus {
    border-color: var(--color-accent);
  }

  .count {
    margin-left: auto;
    color: var(--color-text-muted);
    font-size: var(--font-size-small);
    white-space: nowrap;
  }

  button,
  select {
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

  button.on {
    border-color: var(--color-accent);
    color: var(--color-accent);
  }

  button.primary {
    background: var(--color-accent);
    border-color: var(--color-accent);
    color: var(--color-on-accent);
  }

  button:focus-visible,
  select:focus-visible {
    outline: 1px solid var(--color-accent);
    outline-offset: 1px;
  }
</style>
