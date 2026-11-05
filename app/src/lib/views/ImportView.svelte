<script lang="ts">
  import { untrack } from "svelte";
  import { actionLabel, runAction } from "../actions/registry";
  import { countOf } from "../lists/format";
  import { pickFolder } from "../platform/dialog";
  import { app } from "../state/app.svelte";

  const preview = $derived(app.importPreview);
  const report = $derived(app.importReport);
  const importing = $derived(app.scan?.kind === "import");

  // Where each exported folder lives here. A null means it is not on this machine.
  let chosen = $state<Record<string, string | null>>(
    untrack(() =>
      Object.fromEntries(
        (app.importPreview?.roots ?? []).map((root) => [root.id, root.suggestedPath]),
      ),
    ),
  );

  async function choose(id: string, name: string): Promise<void> {
    const path = await pickFolder(`Where is ${name} on this computer?`);
    if (path) chosen[id] = path;
  }

  function start(): void {
    if (!preview) return;
    void runAction("library.startImport", {
      path: preview.path,
      roots: preview.roots.map((root) => ({ id: root.id, path: chosen[root.id] ?? null })),
    });
  }
</script>

<div class="page">
  <h1>{actionLabel("library.chooseImport")}</h1>

  {#if preview}
    <p class="meta">
      {preview.path}<br />
      {countOf(preview.trackCount, "track")}, {countOf(preview.playlistCount, "playlist")},
      {countOf(preview.ratedTrackCount, "rated track")}. Exported
      {new Date(preview.exportedAt).toLocaleString()}.
    </p>

    <h2>Where are these folders on this computer?</h2>
    <p class="meta">
      Tracks in a folder you skip are kept and marked missing, with their ratings and playlist
      entries. Importing again later can find them. Your music files are only read.
    </p>
    {#each preview.roots as root (root.id)}
      <div class="root">
        <div class="details">
          <div class="name">{root.name} ({countOf(root.trackCount, "track")})</div>
          <div class="meta">Was at {root.path}</div>
          <div class="target" class:unset={!chosen[root.id]}>
            {chosen[root.id] ?? "Not on this computer"}
          </div>
        </div>
        <button disabled={importing} onclick={() => choose(root.id, root.name)}>Choose folder</button>
        <button disabled={importing || !chosen[root.id]} onclick={() => (chosen[root.id] = null)}>
          Skip
        </button>
      </div>
    {/each}

    <div class="actions">
      <button class="primary" disabled={app.scan !== null} onclick={start}>
        {actionLabel("library.startImport")}
      </button>
      <span class="meta">
        Existing ratings, playlists and settings here are kept; the import only fills in what is
        missing.
      </span>
    </div>
  {/if}

  {#if report}
    <section class="report" aria-label="Import result">
      <h2>{report.canceled ? "Import canceled" : "Import finished"}</h2>
      {#if report.canceled}
        <p class="meta">No ratings or playlists were changed. You can run it again.</p>
      {:else}
        <ul>
          <li>
            {countOf(report.tracks, "track")}: {(
              report.matchedById +
              report.matchedByPath +
              report.matchedByFingerprint
            ).toLocaleString()} found
            ({report.matchedById.toLocaleString()} by ID, {report.matchedByPath.toLocaleString()} by
            path, {report.matchedByFingerprint.toLocaleString()} by fingerprint)
          </li>
          <li>{report.missing.toLocaleString()} missing</li>
          <li>
            {countOf(report.playlistsAdded, "playlist")} added,
            {report.playlistsKept.toLocaleString()} already here
          </li>
          <li>
            Ratings and play counts: {report.trackDataAdded.toLocaleString()} restored,
            {report.trackDataKept.toLocaleString()} kept as they were
          </li>
          <li>{report.historyAdded.toLocaleString()} plays added to history</li>
          <li>{countOf(report.settingsAdded, "setting")} added</li>
          {#if report.unknownReferences > 0}
            <li class="problem">
              {report.unknownReferences.toLocaleString()} references to tracks the export does not list
            </li>
          {/if}
        </ul>
        {#if report.missingPaths.length > 0}
          <h2>Missing files</h2>
          <ul class="paths">
            {#each report.missingPaths as path (path)}
              <li>{path}</li>
            {/each}
            {#if report.missing > report.missingPaths.length}
              <li>and {(report.missing - report.missingPaths.length).toLocaleString()} more</li>
            {/if}
          </ul>
        {/if}
      {/if}
    </section>
  {/if}
</div>

<style>
  .page {
    padding: var(--space-md);
    overflow-y: auto;
    min-height: 0;
  }

  h1 {
    margin: 0 0 var(--space-sm);
    font-size: var(--font-size-title);
    font-weight: 600;
  }

  h2 {
    margin: var(--space-md) 0 var(--space-xs);
    font-size: var(--font-size-body);
    font-weight: 600;
  }

  p {
    margin: 0 0 var(--space-sm);
  }

  .meta {
    color: var(--color-text-muted);
    font-size: var(--font-size-small);
    overflow-wrap: anywhere;
  }

  .root {
    display: flex;
    align-items: center;
    gap: var(--space-sm);
    padding: var(--space-sm) var(--space-md);
    margin-bottom: var(--space-sm);
    background: var(--color-surface);
    border: 1px solid var(--color-border);
    border-radius: var(--radius);
  }

  .details {
    flex: 1;
    min-width: 0;
  }

  .name {
    font-weight: 600;
  }

  .target {
    overflow-wrap: anywhere;
  }

  .target.unset {
    color: var(--color-danger);
  }

  .actions {
    display: flex;
    align-items: center;
    gap: var(--space-sm);
    margin-top: var(--space-md);
  }

  .report {
    margin-top: var(--space-md);
    padding-top: var(--space-xs);
    border-top: 1px solid var(--color-border);
  }

  ul {
    margin: 0;
    padding-left: var(--space-md);
  }

  .paths {
    color: var(--color-text-muted);
    font-size: var(--font-size-small);
    overflow-wrap: anywhere;
  }

  .problem {
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

  button:hover:enabled {
    background: var(--color-hover);
  }

  button:disabled {
    color: var(--color-text-muted);
    cursor: default;
  }

  button:focus-visible {
    outline: 1px solid var(--color-accent);
    outline-offset: 1px;
  }

  button.primary:enabled {
    background: var(--color-accent);
    border-color: var(--color-accent);
    color: var(--color-on-accent);
    flex: none;
  }
</style>
