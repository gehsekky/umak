<script lang="ts">
  import { untrack } from "svelte";
  import { actionLabel, runAction } from "../actions/registry";
  import { call } from "../api/client";
  import type { TagEdit } from "../api/generated/TagEdit";
  import { countOf } from "../lists/format";
  import { app } from "../state/app.svelte";
  import { tagDraft } from "../tags/current.svelte";
  import { canUndo, describeEdit } from "../tags/plan";

  const PAGE = 50;

  let edits = $state.raw<TagEdit[]>([]);
  let total = $state(0);
  let shown = $state(PAGE);
  let loaded = $state(false);
  let loading = 0;

  async function load(): Promise<void> {
    const token = ++loading;
    const page = await call("tags.edits.list", { offset: 0, limit: shown });
    if (token !== loading) return;
    edits = page.edits;
    total = page.total;
    loaded = true;
  }

  $effect(() => {
    void app.tagEditsVersion;
    void shown;
    untrack(() => {
      load().catch((failure: { message?: string }) => app.fail(failure.message ?? String(failure)));
    });
  });

  function title(edit: TagEdit): string {
    const kind = edit.undoes ? "Undo" : "Edit";
    if (edit.status === "writing") return `${kind} being written`;
    if (edit.status === "canceled") return `${kind}, canceled`;
    if (edit.status === "interrupted") return `${kind}, interrupted`;
    return kind;
  }
</script>

<div class="page">
  <h1>{actionLabel("nav.tagEdits")}</h1>

  {#if app.waitingEdit}
    <div class="banner">
      <span>A preview is waiting to be written or discarded.</span>
      <button onclick={() => runAction("nav.tagReview", { id: app.waitingEdit as string })}>
        Open it
      </button>
    </div>
  {:else if tagDraft.active}
    <div class="banner">
      <span>You were editing {countOf(tagDraft.tracks.length, "track")}.</span>
      <button onclick={() => runAction("nav.tagEditor")}>Continue</button>
      <button onclick={() => runAction("tags.closeDraft")}>{actionLabel("tags.closeDraft")}</button>
    </div>
  {/if}

  {#if loaded && edits.length === 0}
    <p class="meta">
      No edits yet. To edit tags, select tracks in the library and choose
      {actionLabel("tags.edit")}.
    </p>
  {/if}

  {#each edits as edit (edit.id)}
    <div class="edit">
      <div class="details">
        <div class="name">{title(edit)}</div>
        <div class="meta">
          {new Date(edit.finishedAt ?? edit.createdAt).toLocaleString()} · {describeEdit(edit)}
          {#if edit.undoneBy}· undone{/if}
        </div>
      </div>
      <button onclick={() => runAction("nav.tagReview", { id: edit.id })}>Details</button>
      <button
        disabled={!canUndo(edit) || edit.undoneBy !== null || app.scan !== null}
        onclick={() => runAction("tags.undo", { id: edit.id })}
      >
        {actionLabel("tags.undo")}
      </button>
    </div>
  {/each}

  {#if edits.length < total}
    <button class="more" onclick={() => (shown += PAGE)}>
      Show more ({(total - edits.length).toLocaleString()} left)
    </button>
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

  .meta {
    margin: 0;
    color: var(--color-text-muted);
    font-size: var(--font-size-small);
    overflow-wrap: anywhere;
  }

  .banner,
  .edit {
    display: flex;
    align-items: center;
    gap: var(--space-sm);
    padding: var(--space-sm) var(--space-md);
    margin-bottom: var(--space-sm);
    background: var(--color-surface);
    border: 1px solid var(--color-border);
    border-radius: var(--radius);
  }

  .banner {
    border-color: var(--color-accent);
  }

  .banner span {
    flex: 1;
    min-width: 0;
  }

  .details {
    flex: 1;
    min-width: 0;
  }

  .name {
    font-weight: 600;
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

  button:focus-visible {
    outline: 1px solid var(--color-accent);
    outline-offset: 1px;
  }

  .more {
    display: block;
    margin: var(--space-sm) auto;
  }
</style>
