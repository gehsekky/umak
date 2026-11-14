<script lang="ts">
  // One artist: their albums along the top, and all their songs below with the track list's
  // actions. Songs are theirs as the artist list counts them: by album artist where an album
  // has one, and otherwise by track artist.
  import { onDestroy, untrack } from "svelte";
  import { actionLabel, runAction } from "../actions/registry";
  import { call } from "../api/client";
  import type { AlbumRow } from "../api/generated/AlbumRow";
  import type { ArtistAlbums } from "../api/generated/ArtistAlbums";
  import type { TrackRow } from "../api/generated/TrackRow";
  import type { TrackSort } from "../api/generated/TrackSort";
  import { activateTrack } from "../lists/activate";
  import Cover from "../lists/Cover.svelte";
  import { countOf } from "../lists/format";
  import { PagedList, type Source } from "../lists/pagedList.svelte";
  import { Selection } from "../lists/selection.svelte";
  import TrackActions from "../lists/TrackActions.svelte";
  import TrackTable, { type SortState } from "../lists/TrackTable.svelte";
  import { app } from "../state/app.svelte";

  /** The most tracks the core gives in one page. */
  const MAX_PAGE = 1000;
  /** How often covers are looked for again while the art cache fills. */
  const ART_RELOAD_MS = 1000;
  const INITIAL_SORT: SortState = { sort: "album", descending: false };

  let detail = $state.raw<ArtistAlbums | null>(null);
  let error = $state<string | null>(null);
  let sort = $state<SortState>(INITIAL_SORT);
  let table = $state<TrackTable<TrackRow>>();
  let artTimer: ReturnType<typeof setTimeout> | undefined;

  const id = $derived(app.view.kind === "artist" ? app.view.id : null);

  function page(artistId: string, order: SortState, offset: number, limit: number) {
    return call("library.tracks.page", {
      query: null,
      artist: artistId,
      sort: order.sort,
      descending: order.descending,
      offset,
      limit,
    });
  }

  function source(artistId: string | null, order: SortState): Source<TrackRow> {
    return async (offset, limit) => {
      if (artistId === null) return { rows: [], total: 0 };
      const found = await page(artistId, order, offset, limit);
      return { rows: found.tracks, total: found.total };
    };
  }

  /** All the artist's songs, in the order the list shows them. */
  async function allTrackIds(): Promise<string[]> {
    if (id === null) return [];
    const ids: string[] = [];
    for (let offset = 0; ; offset += MAX_PAGE) {
      const found = await page(id, sort, offset, MAX_PAGE);
      ids.push(...found.tracks.map((track) => track.id));
      if (found.tracks.length < MAX_PAGE || ids.length >= found.total) return ids;
    }
  }

  const list = new PagedList<TrackRow>(source(null, INITIAL_SORT), (row) => row.id);
  const selection = new Selection();

  async function load(artistId: string): Promise<void> {
    try {
      const found = await call("library.artists.get", { id: artistId });
      if (artistId !== id) return;
      detail = found;
      error = null;
      const pending = found.albums.filter((album) => album.artPending).map((album) => album.id);
      if (pending.length > 0) void call("library.albums.findArt", { ids: pending.slice(0, 200) });
    } catch (failure) {
      error = (failure as { message?: string }).message ?? String(failure);
    }
  }

  $effect(() => {
    const artistId = id;
    if (artistId === null) return;
    untrack(() => {
      detail = null;
      sort = INITIAL_SORT;
      selection.clear();
      list.setSource(source(artistId, sort));
      void load(artistId);
    });
  });

  $effect(() => {
    void app.libraryVersion;
    untrack(() => {
      if (id === null || !detail) return;
      void load(id);
      if (list.ready) void list.reload();
    });
  });

  // Covers may have been found since the artist opened.
  $effect(() => {
    void app.artVersion;
    untrack(() => {
      if (artTimer !== undefined || id === null) return;
      if (!detail?.albums.some((album) => album.artPending)) return;
      artTimer = setTimeout(() => {
        artTimer = undefined;
        if (id !== null) void load(id);
      }, ART_RELOAD_MS);
    });
  });

  onDestroy(() => clearTimeout(artTimer));

  function choose(next: TrackSort): void {
    sort = {
      sort: next,
      descending: sort.sort === next ? !sort.descending : false,
    };
    list.setSource(source(id, sort));
    selection.clear();
    table?.scrollToTop();
  }

  async function playAll(how: "play" | "next" | "queue"): Promise<void> {
    const trackIds = await allTrackIds();
    if (trackIds.length === 0) return;
    if (how === "play") void runAction("player.playTracks", { trackIds, startIndex: 0 });
    else void runAction(how === "next" ? "queue.playNext" : "queue.add", { trackIds });
  }

  function openAlbum(album: AlbumRow): void {
    if (!detail) return;
    const { id: artistId, name } = detail.artist;
    void runAction("nav.album", { id: album.id, from: { id: artistId, name } });
  }

  /** Plays the artist's songs from this one, as the list orders them. */
  function activate(row: TrackRow, index: number): void {
    void activateTrack(
      row.id,
      () => [...selection.keys] as string[],
      async () => {
        const trackIds = await allTrackIds();
        const startIndex = trackIds[index] === row.id ? index : Math.max(trackIds.indexOf(row.id), 0);
        return runAction("player.playTracks", { trackIds, startIndex });
      },
    );
  }
</script>

{#if error}
  <div class="message">
    <p class="error">{error}</p>
    <button onclick={() => runAction("nav.artists")}>{actionLabel("nav.artists")}</button>
  </div>
{:else if detail}
  {@const artist = detail.artist}
  <header>
    <button class="back" onclick={() => runAction("nav.artists")}>← {actionLabel("nav.artists")}</button>
    <h1 title={artist.name}>{artist.name}</h1>
    <p class="meta">
      {countOf(artist.albumCount, "album")} · {countOf(artist.trackCount, "song")}
    </p>
    <div class="buttons">
      <button class="primary" onclick={() => playAll("play")}>Play all</button>
      <button onclick={() => playAll("next")}>{actionLabel("queue.playNext")}</button>
      <button onclick={() => playAll("queue")}>{actionLabel("queue.add")}</button>
    </div>
  </header>

  <ul class="albums" aria-label="Albums by {artist.name}">
    {#each detail.albums as album (album.id)}
      <li>
        <button class="album" title={album.title} onclick={() => openAlbum(album)}>
          <span class="cover"><Cover art={album.art} size="small" alt="" /></span>
          <span class="title">{album.title}</span>
          <span class="year">{album.year ?? ""}</span>
        </button>
      </li>
    {/each}
  </ul>

  <TrackActions
    trackIds={[...selection.keys] as string[]}
    hint="Select songs to play them, queue them, edit them or convert them."
  />

  <TrackTable
    bind:this={table}
    {list}
    {selection}
    label="Songs by {artist.name}"
    track={(row) => row}
    number={(row) => (row.trackNo > 0 ? String(row.trackNo) : "")}
    {sort}
    onsort={choose}
    onactivate={activate}
  >
    {#snippet empty()}
      <p>No songs by {artist.name}.</p>
    {/snippet}
  </TrackTable>
{/if}

<style>
  header {
    display: flex;
    flex-direction: column;
    align-items: flex-start;
    gap: var(--space-xs);
    padding: var(--space-md) var(--space-md) var(--space-sm);
  }

  h1 {
    margin: 0;
    max-width: 100%;
    font-size: var(--font-size-title);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  p {
    margin: 0;
  }

  .meta {
    color: var(--color-text-muted);
    font-size: var(--font-size-small);
  }

  .buttons {
    display: flex;
    flex-wrap: wrap;
    gap: var(--space-sm);
    margin-top: var(--space-xs);
  }

  .albums {
    flex: none;
    display: flex;
    gap: var(--space-md);
    margin: 0;
    padding: var(--space-sm) var(--space-md) var(--space-md);
    list-style: none;
    overflow-x: auto;
    scrollbar-color: var(--color-border) transparent;
    border-bottom: 1px solid var(--color-border);
  }

  .albums li {
    flex: none;
    /* Smaller than the album grid's, leaving room for the songs. */
    width: calc(var(--size-album-tile) * 0.75);
  }

  button.album {
    width: 100%;
    display: flex;
    flex-direction: column;
    align-items: stretch;
    gap: 0;
    padding: 0;
    background: none;
    border: none;
    text-align: left;
  }

  button.album:hover {
    background: none;
  }

  .cover {
    display: block;
    aspect-ratio: 1;
  }

  button.album:hover .cover :global(.cover) {
    border-color: var(--color-text-muted);
  }

  .title,
  .year {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .title {
    margin-top: var(--space-xs);
    font-size: var(--font-size-body);
  }

  .year {
    min-height: 1.2em;
    color: var(--color-text-muted);
    font-size: var(--font-size-small);
  }

  .message {
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: var(--space-sm);
    padding: var(--space-lg);
  }

  .error {
    color: var(--color-danger);
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

  button.primary {
    background: var(--color-accent);
    border-color: var(--color-accent);
    color: var(--color-on-accent);
  }

  button.back {
    padding-inline: 0;
    background: none;
    border: none;
    color: var(--color-text-muted);
  }

  button.back:hover {
    color: var(--color-text);
  }

  button:focus-visible {
    outline: 1px solid var(--color-accent);
    outline-offset: 1px;
  }
</style>
