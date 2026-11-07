<script lang="ts">
  // Shows files that look like copies of each other. Nothing here changes a file: the person
  // decides what to remove, in their file manager, and a rescan brings the library up to date.
  import { untrack } from "svelte";
  import { actionLabel, runAction } from "../actions/registry";
  import { call } from "../api/client";
  import type { DuplicateGroup } from "../api/generated/DuplicateGroup";
  import type { DuplicateMatch } from "../api/generated/DuplicateMatch";
  import { copyFacts, fullPath, searchText, tagsDiffer, tagsOf } from "../duplicates/show";
  import { countOf, formatDuration, formatQuality } from "../lists/format";
  import { app } from "../state/app.svelte";

  /** Enough to look through at once; more arrive on request. */
  const PAGE = 100;
  const MAX_REQUEST = 1000;

  const TABS: { by: DuplicateMatch; label: string; about: string }[] = [
    {
      by: "audio",
      label: "Same audio",
      about:
        "Files whose audio matches by its length and its opening, whatever their tags say. The same song encoded twice does not match here.",
    },
    {
      by: "tags",
      label: "Same tags",
      about:
        "Files with the same title, artist, album and track number, exactly as written. They can be different encodes or recordings, so compare them before removing one.",
    },
  ];

  let groups = $state.raw<DuplicateGroup[]>([]);
  let totals = $state<Record<DuplicateMatch, number | null>>({ audio: null, tags: null });
  let totalTracks = $state(0);
  let shown = $state(PAGE);
  let loaded = $state(false);
  let loadError = $state<string | null>(null);
  /** Replaced to ignore an answer that a newer load has overtaken. */
  let loading = 0;

  const by = $derived(app.duplicatesBy);
  const tab = $derived(TABS.find((one) => one.by === by) ?? TABS[0]);
  const total = $derived(totals[by] ?? 0);

  async function load(): Promise<void> {
    const token = ++loading;
    const wanted = by;
    const other: DuplicateMatch = wanted === "audio" ? "tags" : "audio";
    const found: DuplicateGroup[] = [];
    let count = 0;
    let tracks = 0;
    do {
      const page = await call("duplicates.list", {
        by: wanted,
        offset: found.length,
        limit: Math.min(shown - found.length, MAX_REQUEST),
      });
      if (token !== loading) return;
      found.push(...page.groups);
      count = page.totalGroups;
      tracks = page.totalTracks;
      if (page.groups.length === 0) break;
    } while (found.length < Math.min(shown, count));
    const otherPage = await call("duplicates.list", { by: other, offset: 0, limit: 0 });
    if (token !== loading) return;
    groups = found;
    totalTracks = tracks;
    totals = { ...totals, [wanted]: count, [other]: otherPage.totalGroups };
    loaded = true;
    loadError = null;
  }

  // A rescan or a tag edit can change what matches.
  $effect(() => {
    void by;
    void shown;
    void app.libraryVersion;
    untrack(() => {
      load().catch((failure: { message?: string }) => {
        loadError = failure.message ?? String(failure);
      });
    });
  });

  function choose(next: DuplicateMatch): void {
    if (next === by) return;
    app.duplicatesBy = next;
    groups = [];
    shown = PAGE;
    loaded = false;
  }

  function ids(group: DuplicateGroup): string[] {
    return group.tracks.map((copy) => copy.track.id);
  }

  function rootPath(id: string): string | undefined {
    return app.roots.find((root) => root.id === id)?.path;
  }
</script>

<div class="page">
  <header>
    <h1>{actionLabel("nav.duplicates")}</h1>
    {#if loaded && total > 0}
      <span class="meta">{countOf(total, "group")}, {countOf(totalTracks, "file")}</span>
    {/if}
  </header>

  <div class="tabs" role="group" aria-label="How to compare files">
    {#each TABS as one (one.by)}
      <button class:on={one.by === by} aria-pressed={one.by === by} onclick={() => choose(one.by)}>
        {one.label}{totals[one.by] === null ? "" : ` (${totals[one.by]?.toLocaleString()})`}
      </button>
    {/each}
    <span class="spacer"></span>
    <button disabled={app.scan !== null} onclick={() => runAction("library.rescan")}>
      {actionLabel("library.rescan")}
    </button>
  </div>
  <p class="meta about">
    {tab.about} Umak does not remove files: remove a copy in your file manager, then rescan.
  </p>

  {#if loadError}
    <p class="error">Could not look for duplicates: {loadError}</p>
  {/if}

  <div class="list" role="list" aria-label="Groups of duplicate files">
    {#each groups as group (group.tracks[0].track.id)}
      {@const first = group.tracks[0].track}
      {@const differ = tagsDiffer(group.tracks)}
      <article class="group" role="listitem">
        <div class="head">
          <span class="title">{first.title}</span>
          <span class="meta">{first.artist}</span>
          <span class="meta">{first.album}</span>
          <span class="spacer"></span>
          <button onclick={() => runAction("library.showMatches", { query: searchText(first) })}>
            {actionLabel("library.showMatches")}
          </button>
          <button onclick={() => runAction("tags.edit", { trackIds: ids(group) })}>
            {actionLabel("tags.edit")}
          </button>
        </div>
        {#each group.tracks as copy, index (copy.track.id)}
          {@const path = fullPath(rootPath(copy.rootId), copy.relPath)}
          <div class="copy">
            <button
              aria-label="Play {path}"
              onclick={() =>
                runAction("player.playTracks", { trackIds: ids(group), startIndex: index })}
            >
              Play
            </button>
            <div class="where">
              <div class="path">{path}</div>
              {#if differ}<div class="meta">{tagsOf(copy.track)}</div>{/if}
            </div>
            <span class="facts meta">
              {copyFacts(copy, formatQuality(copy.track), formatDuration(copy.track.durationMs))}
            </span>
          </div>
        {/each}
      </article>
    {/each}
    {#if groups.length < total}
      <button class="more" onclick={() => (shown += PAGE)}>
        Show more ({(total - groups.length).toLocaleString()} left)
      </button>
    {/if}
    {#if loaded && total === 0 && !loadError}
      <p class="meta none">
        {app.roots.length === 0
          ? "Add a folder to start your library."
          : "No duplicates found this way."}
      </p>
    {/if}
  </div>
</div>

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

  p {
    margin: 0;
  }

  .meta {
    color: var(--color-text-muted);
    font-size: var(--font-size-small);
  }

  .error {
    color: var(--color-danger);
    padding: var(--space-sm) var(--space-md);
  }

  .tabs {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: var(--space-sm);
    padding: 0 var(--space-md) var(--space-sm);
  }

  .spacer {
    flex: 1;
  }

  .about {
    padding: 0 var(--space-md) var(--space-sm);
    border-bottom: 1px solid var(--color-border);
  }

  .list {
    flex: 1;
    min-height: 0;
    overflow-y: auto;
    padding: var(--space-sm) var(--space-md);
  }

  .group {
    padding: var(--space-sm);
    margin-bottom: var(--space-xs);
    background: var(--color-surface);
    border: 1px solid var(--color-border);
    border-radius: var(--radius);
  }

  .head {
    display: flex;
    flex-wrap: wrap;
    align-items: baseline;
    gap: var(--space-xs) var(--space-sm);
    min-width: 0;
  }

  .title {
    font-weight: 600;
    overflow-wrap: anywhere;
  }

  .copy {
    display: flex;
    flex-wrap: wrap;
    align-items: baseline;
    gap: var(--space-xs) var(--space-sm);
    margin-top: var(--space-xs);
    padding-top: var(--space-xs);
    border-top: 1px solid var(--color-border);
  }

  .where {
    flex: 1 1 20em;
    min-width: 0;
  }

  .path {
    overflow-wrap: anywhere;
  }

  .facts {
    flex: none;
    max-width: 100%;
  }

  .none {
    padding: var(--space-md) 0;
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

  button.on {
    border-color: var(--color-accent);
    color: var(--color-accent);
  }

  button:focus-visible {
    outline: 1px solid var(--color-accent);
    outline-offset: 1px;
  }

  .more {
    display: block;
    margin: var(--space-sm) auto;
  }
</style>
