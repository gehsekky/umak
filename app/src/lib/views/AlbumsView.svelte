<script lang="ts" module>
  import type { AlbumSort } from "../api/generated/AlbumSort";

  type Order = { sort: AlbumSort; descending: boolean };

  /**
   * Kept between visits, so coming back from an album finds the grid as it was left. It
   * lives here rather than in the component, which cannot save it as it closes: by then it
   * reads the values from before the change that closed it.
   */
  const kept = $state<{ query: string; order: Order; cursor: number }>({
    query: "",
    order: { sort: "artist", descending: false },
    cursor: 0,
  });
</script>

<script lang="ts">
  import { onDestroy, tick, untrack } from "svelte";
  import { actionLabel, runAction } from "../actions/registry";
  import { artUrl, call } from "../api/client";
  import type { AlbumRow } from "../api/generated/AlbumRow";
  import { countOf } from "../lists/format";
  import { gridMove } from "../lists/grid";
  import { PagedList, type Source } from "../lists/pagedList.svelte";
  import VirtualGrid from "../lists/VirtualGrid.svelte";
  import { app } from "../state/app.svelte";

  /** How often covers are looked for again while the art cache fills. */
  const ART_RELOAD_MS = 1000;
  /** How long the grid rests before asking for the covers it shows, so scrolling past asks nothing. */
  const FIND_ART_MS = 250;
  const SORTS: { sort: AlbumSort; label: string }[] = [
    { sort: "artist", label: "Artist" },
    { sort: "title", label: "Title" },
    { sort: "year", label: "Year" },
  ];

  let start = $state(0);
  let end = $state(0);
  let grid = $state<VirtualGrid>();
  let searchBox = $state<HTMLInputElement>();
  /** Pictures that would not load, shown as albums without art. */
  let broken = $state.raw<ReadonlySet<string>>(new Set());
  let artTimer: ReturnType<typeof setTimeout> | undefined;
  let findTimer: ReturnType<typeof setTimeout> | undefined;

  function source(text: string, wanted: Order): Source<AlbumRow> {
    return async (offset, limit) => {
      const page = await call("library.albums.page", {
        query: text.trim() || null,
        sort: wanted.sort,
        descending: wanted.descending,
        offset,
        limit,
      });
      return { rows: page.albums, total: page.total };
    };
  }

  const list = new PagedList<AlbumRow>(source(kept.query, kept.order), (row) => row.id);

  $effect(() => {
    void list.generation;
    void list.ensure(start, Math.max(end, 1));
  });

  function showContents(): void {
    list.setSource(source(kept.query, kept.order));
    kept.cursor = 0;
    grid?.scrollToTop();
  }

  /** A second press reverses the order. Years start with the newest. */
  function choose(sort: AlbumSort): void {
    kept.order = {
      sort,
      descending: kept.order.sort === sort ? !kept.order.descending : sort === "year",
    };
    showContents();
  }

  // Back from an album, the grid shows where it was.
  let revealed = false;
  $effect(() => {
    if (!list.ready || revealed) return;
    revealed = true;
    const index = Math.min(kept.cursor, Math.max(list.total - 1, 0));
    if (index > 0) void tick().then(() => grid?.reveal(index));
  });

  $effect(() => {
    void app.libraryVersion;
    untrack(() => {
      if (list.ready) void list.reload();
    });
  });

  /** Albums on screen still waiting for a cover, which the art cache looks for first. */
  $effect(() => {
    const waiting: string[] = [];
    for (let index = start; index < end; index++) {
      const album = list.row(index);
      if (album?.artPending) waiting.push(album.id);
    }
    clearTimeout(findTimer);
    if (waiting.length === 0) return;
    findTimer = setTimeout(() => {
      void call("library.albums.findArt", { ids: waiting });
    }, FIND_ART_MS);
  });

  /** While the art cache works, covers on screen that were missing may have been found. */
  $effect(() => {
    void app.artVersion;
    untrack(() => {
      if (artTimer !== undefined || !list.ready) return;
      artTimer = setTimeout(() => {
        artTimer = undefined;
        for (let index = start; index < end; index++) {
          if (list.row(index)?.artPending) {
            void list.reload();
            return;
          }
        }
      }, ART_RELOAD_MS);
    });
  });

  onDestroy(() => {
    clearTimeout(artTimer);
    clearTimeout(findTimer);
  });

  $effect(() => {
    if (app.searchFocusRequests > 0) {
      searchBox?.focus();
      searchBox?.select();
    }
  });

  function open(index: number): void {
    const album = list.row(index);
    if (!album) return;
    kept.cursor = index;
    void runAction("nav.album", { id: album.id });
  }

  function keydown(event: KeyboardEvent): void {
    if (event.key === "Enter") {
      event.preventDefault();
      open(kept.cursor);
      return;
    }
    if (!grid) return;
    const next = gridMove(event.key, kept.cursor, grid.columns(), list.total, grid.pageRows());
    if (next === null) return;
    event.preventDefault();
    kept.cursor = next;
    grid.reveal(next);
  }

  function markBroken(art: string): void {
    broken = new Set([...broken, art]);
  }
</script>

<div class="toolbar">
  <input
    bind:this={searchBox}
    bind:value={kept.query}
    class="search"
    type="search"
    placeholder="Search albums by title, artist, track or genre (Ctrl+F)"
    aria-label={actionLabel("library.focusSearch")}
    oninput={showContents}
    onkeydown={(event) => {
      if (event.key === "Escape" && kept.query) {
        kept.query = "";
        showContents();
      } else if (event.key === "ArrowDown") {
        event.preventDefault();
        grid?.focus();
      }
    }}
  />
  <div class="sorts" role="group" aria-label="Sort albums">
    {#each SORTS as choice (choice.sort)}
      <button
        class:on={kept.order.sort === choice.sort}
        aria-pressed={kept.order.sort === choice.sort}
        onclick={() => choose(choice.sort)}
      >
        {choice.label}
        {#if kept.order.sort === choice.sort}{kept.order.descending ? "↓" : "↑"}{/if}
      </button>
    {/each}
  </div>
  <span class="count">{list.ready ? countOf(list.total, "album") : ""}</span>
</div>

{#if list.error}
  <p class="message error">{list.error}</p>
{:else if list.ready && list.total === 0}
  <div class="message">
    {#if kept.query.trim()}
      <p>No albums match "{kept.query.trim()}"</p>
    {:else if app.roots.length === 0}
      <p>Add a folder to start your library.</p>
      <button class="primary" onclick={() => runAction("library.addFolder")}>
        {actionLabel("library.addFolder")}
      </button>
    {:else if app.scan}
      <p>Scanning your folders...</p>
    {:else}
      <p>No albums found in your folders.</p>
    {/if}
  </div>
{:else}
  <div class="grid">
  <VirtualGrid bind:this={grid} bind:start bind:end total={list.total} label="Albums" onkeydown={keydown}>
    {#snippet tile(index)}
      {@const album = list.row(index)}
      <div
        class="tile"
        class:cursor={index === kept.cursor}
        role="gridcell"
        tabindex="-1"
        aria-selected={index === kept.cursor}
        onclick={() => open(index)}
        onkeydown={() => {}}
      >
        <div class="cover">
          {#if album?.art && !broken.has(album.art)}
            {@const art = album.art}
            <img
              src={artUrl(art, "small")}
              alt=""
              decoding="async"
              draggable="false"
              onerror={() => markBroken(art)}
            />
          {:else}
            <span class="placeholder" aria-hidden="true">♪</span>
          {/if}
          {#if album}
            <button
              class="play"
              title={actionLabel("albums.play")}
              aria-label="{actionLabel('albums.play')}: {album.title}"
              onclick={(event) => {
                event.stopPropagation();
                void runAction("albums.play", { id: album.id });
              }}>▶</button
            >
          {/if}
        </div>
        <div class="title" title={album?.title}>{album?.title ?? ""}</div>
        <div class="artist" title={album?.artist}>
          {album?.artist ?? ""}{album?.year ? ` · ${album.year}` : ""}
        </div>
      </div>
    {/snippet}
  </VirtualGrid>
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

  .sorts {
    display: flex;
    gap: var(--space-xs);
  }

  .count {
    margin-left: auto;
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

  button:focus-visible {
    outline: 1px solid var(--color-accent);
    outline-offset: 1px;
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

  .tile {
    height: 100%;
    display: flex;
    flex-direction: column;
    cursor: pointer;
    outline: none;
  }

  .cover {
    position: relative;
    aspect-ratio: 1;
    display: flex;
    align-items: center;
    justify-content: center;
    background: var(--color-surface-raised);
    border: 1px solid var(--color-border);
    border-radius: var(--radius);
    overflow: hidden;
  }

  .tile:hover .cover {
    border-color: var(--color-text-muted);
  }

  .grid {
    flex: 1;
    min-height: 0;
    display: flex;
    flex-direction: column;
  }

  /* The cursor shows only while the grid has the keyboard. */
  .grid:focus-within .tile.cursor .cover {
    border-color: var(--color-accent);
    outline: 1px solid var(--color-accent);
  }

  .cover img {
    width: 100%;
    height: 100%;
    object-fit: contain;
  }

  .placeholder {
    color: var(--color-text-muted);
    font-size: var(--font-size-title);
  }

  .play {
    position: absolute;
    right: var(--space-sm);
    bottom: var(--space-sm);
    width: calc(var(--size-row) + var(--space-sm));
    height: calc(var(--size-row) + var(--space-sm));
    padding: 0;
    border-radius: 50%;
    background: var(--color-accent);
    border-color: var(--color-accent);
    color: var(--color-on-accent);
    opacity: 0;
  }

  .tile:hover .play,
  .play:focus-visible {
    opacity: 1;
  }

  .play:hover {
    background: var(--color-accent);
  }

  .title,
  .artist {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .title {
    margin-top: var(--space-xs);
  }

  .artist {
    color: var(--color-text-muted);
    font-size: var(--font-size-small);
  }
</style>
