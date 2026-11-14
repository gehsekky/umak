<script lang="ts" module>
  /**
   * Kept between visits, so coming back from an artist finds the list as it was left. See
   * the albums view for why it lives here.
   */
  const kept = $state<{ query: string; cursor: number }>({ query: "", cursor: 0 });
</script>

<script lang="ts">
  import { tick, untrack } from "svelte";
  import { actionLabel, runAction } from "../actions/registry";
  import { call } from "../api/client";
  import type { ArtistRow } from "../api/generated/ArtistRow";
  import { countOf } from "../lists/format";
  import { gridMove } from "../lists/grid";
  import { PagedList, type Source } from "../lists/pagedList.svelte";
  import VirtualList from "../lists/VirtualList.svelte";
  import { app } from "../state/app.svelte";
  import LibraryTabs from "./LibraryTabs.svelte";

  let start = $state(0);
  let end = $state(0);
  let rows = $state<VirtualList<ArtistRow>>();
  let searchBox = $state<HTMLInputElement>();

  function source(text: string): Source<ArtistRow> {
    return async (offset, limit) => {
      const page = await call("library.artists.page", {
        query: text.trim() || null,
        offset,
        limit,
      });
      return { rows: page.artists, total: page.total };
    };
  }

  const list = new PagedList<ArtistRow>(source(kept.query), (row) => row.id);

  $effect(() => {
    void list.generation;
    void list.ensure(start, Math.max(end, 1));
  });

  function showContents(): void {
    list.setSource(source(kept.query));
    kept.cursor = 0;
    rows?.scrollToTop();
  }

  // Back from an artist, the list shows where it was.
  let revealed = false;
  $effect(() => {
    if (!list.ready || revealed) return;
    revealed = true;
    const index = Math.min(kept.cursor, Math.max(list.total - 1, 0));
    if (index > 0) void tick().then(() => rows?.reveal(index));
  });

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

  function open(index: number): void {
    const artist = list.row(index);
    if (!artist) return;
    kept.cursor = index;
    void runAction("nav.artist", { id: artist.id });
  }

  function keydown(event: KeyboardEvent): void {
    if (event.key === "Enter") {
      event.preventDefault();
      open(kept.cursor);
      return;
    }
    if (!rows) return;
    const next = gridMove(event.key, kept.cursor, 1, list.total, rows.pageRows());
    if (next === null) return;
    event.preventDefault();
    kept.cursor = next;
    rows.reveal(next);
  }

  function counts(artist: ArtistRow): string {
    return `${countOf(artist.albumCount, "album")} · ${countOf(artist.trackCount, "song")}`;
  }
</script>

<div class="toolbar">
  <LibraryTabs />
  <input
    bind:this={searchBox}
    bind:value={kept.query}
    class="search"
    type="search"
    placeholder="Search artists by name (Ctrl+F)"
    aria-label={actionLabel("library.focusSearch")}
    oninput={showContents}
    onkeydown={(event) => {
      if (event.key === "Escape" && kept.query) {
        kept.query = "";
        showContents();
      } else if (event.key === "ArrowDown") {
        event.preventDefault();
        rows?.focus();
      }
    }}
  />
  <span class="count">{list.ready ? countOf(list.total, "artist") : ""}</span>
</div>

{#if list.error}
  <p class="message error">{list.error}</p>
{:else if list.ready && list.total === 0}
  <div class="message">
    {#if kept.query.trim()}
      <p>No artists match "{kept.query.trim()}"</p>
    {:else if app.roots.length === 0}
      <p>Add a folder to start your library.</p>
      <button class="primary" onclick={() => runAction("library.addFolder")}>
        {actionLabel("library.addFolder")}
      </button>
    {:else if app.scan}
      <p>Scanning your folders...</p>
    {:else}
      <p>No artists found in your folders.</p>
    {/if}
  </div>
{:else}
  <div class="list">
    <VirtualList bind:this={rows} bind:start bind:end total={list.total} label="Artists" onkeydown={keydown}>
      {#snippet row(index)}
        {@const artist = list.row(index)}
        <div
          class="row"
          class:cursor={index === kept.cursor}
          role="row"
          tabindex="-1"
          aria-selected={index === kept.cursor}
          onclick={() => open(index)}
          onkeydown={() => {}}
        >
          <span class="name" title={artist?.name}>{artist?.name ?? ""}</span>
          <span class="counts">{artist ? counts(artist) : ""}</span>
        </div>
      {/snippet}
    </VirtualList>
  </div>
{/if}

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

  .message {
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: var(--space-sm);
    padding: var(--space-lg);
    color: var(--color-text-muted);
  }

  .message.error {
    color: var(--color-danger);
  }

  .list {
    flex: 1;
    min-height: 0;
    display: flex;
    flex-direction: column;
  }

  .row {
    height: 100%;
    display: flex;
    align-items: center;
    gap: var(--space-md);
    padding: 0 var(--space-md);
    border-bottom: 1px solid var(--color-border);
    box-sizing: border-box;
    cursor: pointer;
    outline: none;
  }

  .row:hover {
    background: var(--color-hover);
  }

  /* The cursor shows only while the list has the keyboard. */
  .list:focus-within .row.cursor {
    background: var(--color-selected);
    box-shadow: inset 2px 0 0 var(--color-accent);
  }

  .name {
    flex: 1;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .counts {
    color: var(--color-text-muted);
    font-size: var(--font-size-small);
    white-space: nowrap;
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

  button.primary {
    background: var(--color-accent);
    border-color: var(--color-accent);
    color: var(--color-on-accent);
  }
</style>
