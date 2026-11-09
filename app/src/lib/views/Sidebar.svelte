<script lang="ts">
  import { actionLabel, runAction } from "../actions/registry";
  import { app } from "../state/app.svelte";

  let newName = $state("");
  let nameInput = $state<HTMLInputElement>();

  $effect(() => {
    if (app.creatingPlaylist) nameInput?.focus();
  });

  async function create(event: SubmitEvent): Promise<void> {
    event.preventDefault();
    const name = newName.trim();
    if (!name) return;
    if (await runAction("playlists.create", { name })) {
      newName = "";
      app.creatingPlaylist = false;
    }
  }

  function cancelCreate(): void {
    newName = "";
    app.creatingPlaylist = false;
  }
</script>

<div class="section">
  <button
    class="item"
    class:active={app.view.kind === "library"}
    onclick={() => runAction("nav.library")}
  >
    <span>{actionLabel("nav.library")}</span>
    <span class="count">{app.trackCount.toLocaleString()}</span>
  </button>
  <button
    class="item"
    class:active={app.view.kind === "folders"}
    onclick={() => runAction("nav.folders")}
  >
    <span>{actionLabel("nav.folders")}</span>
    <span class="count">{app.roots.length}</span>
  </button>
  <button
    class="item"
    class:active={["tagEdits", "tagEditor", "tagReview"].includes(app.view.kind)}
    onclick={() => runAction("nav.tagEdits")}
  >
    <span>{actionLabel("nav.tagEdits")}</span>
  </button>
  <button
    class="item"
    class:active={app.view.kind === "duplicates"}
    onclick={() => runAction("nav.duplicates")}
  >
    <span>{actionLabel("nav.duplicates")}</span>
  </button>
  <button
    class="item"
    class:active={app.view.kind === "conversions"}
    onclick={() => runAction("nav.conversions")}
  >
    <span>{actionLabel("nav.conversions")}</span>
    {#if app.conversion}
      <span class="count">
        {Math.floor((app.conversion.done * 100) / Math.max(app.conversion.total, 1))}%
      </span>
    {/if}
  </button>
  <button
    class="item"
    class:active={app.view.kind === "settings"}
    onclick={() => runAction("nav.settings")}
  >
    <span>{actionLabel("nav.settings")}</span>
  </button>
</div>

<div class="heading">
  <h2>Playlists</h2>
  <span class="adds">
    <button
      class="add"
      title={actionLabel("nav.smartEditor")}
      aria-label={actionLabel("nav.smartEditor")}
      onclick={() => runAction("nav.smartEditor", { id: null })}>✦</button
    >
    <button
      class="add"
      title="{actionLabel('playlists.startCreate')} (Ctrl+N)"
      aria-label={actionLabel("playlists.startCreate")}
      onclick={() => runAction("playlists.startCreate")}>+</button
    >
  </span>
</div>

<div class="section scroll">
  {#if app.creatingPlaylist}
    <form onsubmit={create}>
      <input
        bind:this={nameInput}
        bind:value={newName}
        placeholder="Playlist name"
        aria-label="Playlist name"
        onblur={() => {
          if (!newName.trim()) cancelCreate();
        }}
        onkeydown={(event) => {
          if (event.key === "Escape") cancelCreate();
        }}
      />
    </form>
  {/if}
  {#each app.playlists as playlist (playlist.id)}
    <button
      class="item"
      class:active={(app.view.kind === "playlist" || app.view.kind === "smartEditor") &&
        app.view.id === playlist.id}
      onclick={() => runAction("nav.playlist", { id: playlist.id })}
    >
      <span class="name" title={playlist.name}>{playlist.name}</span>
      {#if playlist.smart}
        <span class="count" title="Smart playlist" aria-label="Smart playlist">✦</span>
      {:else}
        <span class="count">{(playlist.trackCount ?? 0).toLocaleString()}</span>
      {/if}
    </button>
  {:else}
    {#if !app.creatingPlaylist}
      <p class="hint">No playlists yet</p>
    {/if}
  {/each}
</div>

<style>
  .section {
    padding: var(--space-sm);
    display: flex;
    flex-direction: column;
    gap: 1px;
  }

  .scroll {
    flex: 1;
    min-height: 0;
    overflow-y: auto;
    padding-top: 0;
  }

  .item {
    all: unset;
    display: flex;
    justify-content: space-between;
    gap: var(--space-sm);
    padding: var(--space-xs) var(--space-sm);
    border-radius: var(--radius);
    cursor: pointer;
  }

  .item:hover {
    background: var(--color-hover);
  }

  .item:focus-visible {
    outline: 1px solid var(--color-accent);
  }

  .item.active {
    background: var(--color-selected);
  }

  .name {
    overflow: hidden;
    white-space: nowrap;
    text-overflow: ellipsis;
  }

  .count {
    color: var(--color-text-muted);
    font-size: var(--font-size-small);
    font-variant-numeric: tabular-nums;
    align-self: center;
  }

  .heading {
    display: flex;
    align-items: center;
    justify-content: space-between;
    padding: var(--space-sm) var(--space-md) var(--space-xs);
  }

  h2 {
    margin: 0;
    color: var(--color-text-muted);
    font-size: var(--font-size-small);
    font-weight: 600;
  }

  .adds {
    display: flex;
    gap: var(--space-xs);
  }

  .add {
    all: unset;
    cursor: pointer;
    padding: 0 var(--space-xs);
    border-radius: var(--radius);
    color: var(--color-text-muted);
  }

  .add:hover {
    background: var(--color-hover);
    color: var(--color-text);
  }

  .add:focus-visible {
    outline: 1px solid var(--color-accent);
  }

  input {
    width: 100%;
    box-sizing: border-box;
    padding: var(--space-xs) var(--space-sm);
    margin-bottom: var(--space-xs);
    background: var(--color-bg);
    color: var(--color-text);
    border: 1px solid var(--color-accent);
    border-radius: var(--radius);
    font: inherit;
    outline: none;
  }

  .hint {
    margin: var(--space-xs) var(--space-sm);
    color: var(--color-text-muted);
    font-size: var(--font-size-small);
  }
</style>
