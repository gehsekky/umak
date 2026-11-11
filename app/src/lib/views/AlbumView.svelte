<script lang="ts">
  // One album: its cover and details above its tracks, with the track list's actions.
  import { untrack } from "svelte";
  import { actionLabel, runAction } from "../actions/registry";
  import { call } from "../api/client";
  import type { AlbumTracks } from "../api/generated/AlbumTracks";
  import type { TrackRow } from "../api/generated/TrackRow";
  import Cover from "../lists/Cover.svelte";
  import { countOf, formatTotalTime } from "../lists/format";
  import { PagedList, type Source } from "../lists/pagedList.svelte";
  import { Selection } from "../lists/selection.svelte";
  import TrackActions from "../lists/TrackActions.svelte";
  import TrackTable from "../lists/TrackTable.svelte";
  import { app } from "../state/app.svelte";

  let detail = $state.raw<AlbumTracks | null>(null);
  let error = $state<string | null>(null);

  const id = $derived(app.view.kind === "album" ? app.view.id : null);
  const trackIds = $derived(detail?.tracks.map((track) => track.id) ?? []);
  const discs = $derived(new Set(detail?.tracks.map((track) => track.discNo)).size);
  const totalMs = $derived(
    detail?.tracks.reduce((sum, track) => sum + (track.durationMs ?? 0), 0) ?? 0,
  );

  function source(tracks: TrackRow[]): Source<TrackRow> {
    return async (offset, limit) => ({
      rows: tracks.slice(offset, offset + limit),
      total: tracks.length,
    });
  }

  const list = new PagedList<TrackRow>(source([]), (row) => row.id);
  const selection = new Selection();

  async function load(albumId: string): Promise<void> {
    try {
      const found = await call("library.albums.get", { id: albumId });
      if (albumId !== id) return;
      detail = found;
      error = null;
      list.setSource(source(found.tracks));
      if (found.album.artPending) void call("library.albums.findArt", { ids: [albumId] });
    } catch (failure) {
      error = (failure as { message?: string }).message ?? String(failure);
    }
  }

  $effect(() => {
    const albumId = id;
    if (albumId === null) return;
    untrack(() => {
      detail = null;
      selection.clear();
      void load(albumId);
    });
  });

  $effect(() => {
    void app.libraryVersion;
    untrack(() => {
      if (id !== null && detail) void load(id);
    });
  });

  // The cover may have been found since the album opened.
  $effect(() => {
    void app.artVersion;
    untrack(() => {
      if (id !== null && detail?.album.artPending) void load(id);
    });
  });

  function trackNumber(row: TrackRow): string {
    if (row.trackNo <= 0) return "";
    return discs > 1 ? `${row.discNo}.${String(row.trackNo).padStart(2, "0")}` : String(row.trackNo);
  }
</script>

{#if error}
  <div class="message">
    <p class="error">{error}</p>
    <button onclick={() => runAction("nav.albums")}>{actionLabel("nav.albums")}</button>
  </div>
{:else if detail}
  {@const album = detail.album}
  <header>
    <div class="cover">
      <Cover art={album.art} size="large" alt="Cover of {album.title}" />
    </div>
    <div class="info">
      <button class="back" onclick={() => runAction("nav.albums")}>← {actionLabel("nav.albums")}</button>
      <h1 title={album.title}>{album.title}</h1>
      <p class="artist">{album.artist}</p>
      <p class="meta">
        {[album.year, countOf(album.trackCount, "track"), formatTotalTime(totalMs)]
          .filter((part) => part !== null)
          .join(" · ")}
      </p>
      <div class="buttons">
        <button class="primary" onclick={() => runAction("albums.play", { id: album.id })}>
          {actionLabel("albums.play")}
        </button>
        <button onclick={() => runAction("queue.playNext", { trackIds })}>
          {actionLabel("queue.playNext")}
        </button>
        <button onclick={() => runAction("queue.add", { trackIds })}>
          {actionLabel("queue.add")}
        </button>
      </div>
    </div>
  </header>

  <TrackActions
    trackIds={[...selection.keys] as string[]}
    hint="Select tracks to play them, queue them, edit them or convert them."
    playAlbum={false}
  />

  <TrackTable
    {list}
    {selection}
    label="Tracks of {album.title}"
    track={(row) => row}
    number={trackNumber}
    onactivate={(_row, index) => runAction("player.playTracks", { trackIds, startIndex: index })}
  >
    {#snippet empty()}
      <p>This album has no tracks.</p>
    {/snippet}
  </TrackTable>
{/if}

<style>
  header {
    display: flex;
    gap: var(--space-md);
    padding: var(--space-md);
    border-bottom: 1px solid var(--color-border);
  }

  .cover {
    flex: none;
    width: var(--size-album-cover);
    height: var(--size-album-cover);
  }

  .info {
    min-width: 0;
    display: flex;
    flex-direction: column;
    align-items: flex-start;
    gap: var(--space-xs);
  }

  h1 {
    margin: var(--space-sm) 0 0;
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
    margin-top: auto;
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
