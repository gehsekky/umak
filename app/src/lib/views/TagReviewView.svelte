<script lang="ts">
  // One batch, at any stage: the preview before writing, progress while writing, and the
  // result afterwards. An undo is a batch too, so it comes through here as well.
  import { untrack } from "svelte";
  import { actionLabel, runAction } from "../actions/registry";
  import { call } from "../api/client";
  import type { TagEdit } from "../api/generated/TagEdit";
  import type { TagEditItem } from "../api/generated/TagEditItem";
  import { countOf } from "../lists/format";
  import { app } from "../state/app.svelte";
  import { tagDraft } from "../tags/current.svelte";
  import { canUndo, describeEdit, fieldLabel, isProblem, itemStatusLabel } from "../tags/plan";

  /** Enough to look through at once; more arrive on request. */
  const PAGE = 200;
  const MAX_REQUEST = 1000;

  const id = $derived(app.view.kind === "tagReview" ? app.view.id : "");

  let edit = $state.raw<TagEdit | null>(null);
  let items = $state.raw<TagEditItem[]>([]);
  let total = $state(0);
  let shown = $state(PAGE);
  let problemsOnly = $state(false);
  let loadError = $state<string | null>(null);
  /** Replaced to ignore an answer that a newer load has overtaken. */
  let loading = 0;

  const waiting = $derived(edit?.status === "ready");
  const writing = $derived(edit?.status === "writing");
  const reading = $derived(edit === null || edit.status === "previewing");
  const isUndo = $derived(edit?.undoes !== null && edit?.undoes !== undefined);
  const fromDraft = $derived(tagDraft.active && tagDraft.editId === id);

  async function loadItems(token: number): Promise<void> {
    const found: TagEditItem[] = [];
    let count = 0;
    while (found.length < shown) {
      const page = await call("tags.edits.items", {
        id,
        problems: problemsOnly,
        offset: found.length,
        limit: Math.min(shown - found.length, MAX_REQUEST),
      });
      if (token !== loading) return;
      found.push(...page.items);
      count = page.total;
      if (page.items.length === 0 || found.length >= count) break;
    }
    items = found;
    total = count;
  }

  async function load(): Promise<void> {
    const token = ++loading;
    let current: TagEdit;
    try {
      current = await call("tags.edits.get", { id });
    } catch {
      if (token !== loading) return;
      // A preview's batch appears once its job gets going, and goes if it is canceled.
      const jobs = (await call("jobs.list", {})).jobs;
      if (token !== loading) return;
      if (jobs.some((job) => job.status === "running" && job.kind === "tagPreview")) {
        edit = null;
      } else {
        await runAction("tags.leaveReview");
      }
      return;
    }
    if (token !== loading) return;
    edit = current;
    if (current.status === "previewing") return;
    try {
      await loadItems(token);
      loadError = null;
    } catch (failure) {
      if (token === loading) loadError = (failure as { message?: string }).message ?? String(failure);
    }
  }

  // Going from a batch to its undo stays on this screen, so nothing of the old one may linger.
  let shownId = untrack(() => id);
  $effect(() => {
    if (id !== shownId) {
      shownId = id;
      edit = null;
      items = [];
      total = 0;
      shown = PAGE;
      problemsOnly = false;
      loadError = null;
    }
  });

  // New batch, new contents, or a change in what to show.
  $effect(() => {
    void id;
    void app.tagEditsVersion;
    void app.libraryVersion;
    void problemsOnly;
    void shown;
    untrack(() => void load());
  });

  function values(list: string[]): string {
    return list.join("  |  ");
  }

  function setProblemsOnly(on: boolean): void {
    problemsOnly = on;
    shown = PAGE;
  }

  const hasProblems = $derived(
    edit !== null && edit.blocked + edit.failed + edit.skipped + edit.unsure > 0,
  );

  const heading = $derived.by(() => {
    if (reading) return isUndo ? "Preparing the undo" : "Preparing the preview";
    if (waiting) return isUndo ? "Preview of the undo" : "Preview";
    if (writing) return "Writing tags";
    if (edit?.status === "canceled") return isUndo ? "Undo canceled" : "Edit canceled";
    if (edit?.status === "interrupted") return isUndo ? "Undo interrupted" : "Edit interrupted";
    return isUndo ? "Undo finished" : "Edit finished";
  });
</script>

<div class="page">
  <header>
    <h1>{heading}</h1>
    {#if edit && !reading}
      <p class="summary" data-testid="tag-summary">{describeEdit(edit)}</p>
    {/if}
    {#if (reading || writing) && app.scan}
      <p class="meta">
        {app.scan.done.toLocaleString()} of {app.scan.total.toLocaleString()} files
      </p>
    {/if}
    {#if edit?.status === "canceled" || edit?.status === "interrupted"}
      <p class="meta">Files already written keep their changes. Undo takes them back.</p>
    {/if}
  </header>

  <div class="actions">
    {#if waiting && edit}
      <button
        class="primary"
        disabled={edit.changing === 0 || app.scan !== null}
        onclick={() => runAction("tags.write", { id })}
      >
        {edit.changing === 0
          ? "Nothing to write"
          : `Write ${countOf(edit.changing, "file")}`}
      </button>
      {#if fromDraft}
        <button onclick={() => runAction("tags.backToDraft", { id })}>
          {actionLabel("tags.backToDraft")}
        </button>
      {/if}
      <button onclick={() => runAction("tags.discard", { id })}>
        {actionLabel("tags.discard")}
      </button>
    {:else if (reading || writing) && app.scan}
      <button onclick={() => runAction("library.cancelScan")}>
        {actionLabel("library.cancelScan")}
      </button>
    {:else if edit && !reading && !writing}
      {#if canUndo(edit) && !edit.undoneBy}
        <button disabled={app.scan !== null} onclick={() => runAction("tags.undo", { id })}>
          {actionLabel("tags.undo")}
        </button>
      {:else if edit.undoneBy}
        <span class="meta">This edit has been undone.</span>
      {/if}
      <button class="primary" onclick={() => runAction("tags.leaveReview")}>Done</button>
    {/if}
    <span class="spacer"></span>
    {#if hasProblems}
      <label class="meta">
        <input
          type="checkbox"
          checked={problemsOnly}
          onchange={(event) => setProblemsOnly(event.currentTarget.checked)}
        />
        Only files with problems
      </label>
    {/if}
  </div>

  {#if loadError}
    <p class="error">Could not load this list: {loadError}</p>
  {/if}

  <div class="list" role="list" aria-label="Files in this edit">
    {#each items as item (item.position)}
      <article class="item" role="listitem" class:problem={isProblem(item.status)}>
        <div class="head">
          <span class="title">{item.title}</span>
          <span class="muted">{item.artist}</span>
          <span class="status">{itemStatusLabel(item.status)}</span>
        </div>
        <div class="path">{item.relPath}</div>
        {#if item.message}<div class="message">{item.message}</div>{/if}
        {#if item.changes.length > 0}
          <div class="changes">
            {#each item.changes as change (change.field)}
              <span class="field">{fieldLabel(change.field)}</span>
              <span class="before">{change.before.length > 0 ? values(change.before) : "(empty)"}</span>
              <span class="arrow" aria-label="becomes">→</span>
              <span class="after">{change.after.length > 0 ? values(change.after) : "(removed)"}</span>
            {/each}
          </div>
        {/if}
      </article>
    {/each}
    {#if items.length < total}
      <button class="more" onclick={() => (shown += PAGE)}>
        Show more ({(total - items.length).toLocaleString()} left)
      </button>
    {/if}
    {#if edit && !reading && total === 0 && !loadError}
      <p class="meta none">
        {problemsOnly
          ? "No files with problems."
          : "No file needs changing: they already hold these values."}
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
    padding: var(--space-md) var(--space-md) var(--space-sm);
  }

  h1 {
    margin: 0 0 var(--space-xs);
    font-size: var(--font-size-title);
    font-weight: 600;
  }

  p {
    margin: 0;
  }

  .meta,
  .muted,
  .path {
    color: var(--color-text-muted);
    font-size: var(--font-size-small);
  }

  .error {
    color: var(--color-danger);
    padding: var(--space-sm) var(--space-md);
  }

  .actions {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: var(--space-sm);
    padding: 0 var(--space-md) var(--space-sm);
    border-bottom: 1px solid var(--color-border);
  }

  .spacer {
    flex: 1;
  }

  label {
    display: flex;
    align-items: center;
    gap: var(--space-xs);
  }

  .list {
    flex: 1;
    min-height: 0;
    overflow-y: auto;
    padding: var(--space-sm) var(--space-md);
  }

  .item {
    padding: var(--space-sm);
    margin-bottom: var(--space-xs);
    background: var(--color-surface);
    border: 1px solid var(--color-border);
    border-radius: var(--radius);
  }

  .item.problem {
    border-color: var(--color-danger);
  }

  .head {
    display: flex;
    align-items: baseline;
    gap: var(--space-sm);
    min-width: 0;
  }

  .title {
    font-weight: 600;
    overflow-wrap: anywhere;
  }

  .status {
    flex: none;
    margin-left: auto;
    color: var(--color-text-muted);
    font-size: var(--font-size-small);
  }

  .item.problem .status,
  .item.problem .message {
    color: var(--color-danger);
  }

  .path {
    overflow-wrap: anywhere;
  }

  .message {
    margin-top: var(--space-xs);
    color: var(--color-text-muted);
    font-size: var(--font-size-small);
  }

  .changes {
    display: grid;
    grid-template-columns: auto minmax(0, 1fr) auto minmax(0, 1fr);
    gap: var(--space-xs) var(--space-sm);
    margin-top: var(--space-xs);
  }

  .field,
  .before,
  .arrow {
    color: var(--color-text-muted);
  }

  .before,
  .after {
    overflow-wrap: anywhere;
    /* A value's own spaces are part of it, and the gap between values must show. */
    white-space: pre-wrap;
  }

  .none {
    padding: var(--space-md) 0;
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

  button:hover:not(:disabled) {
    background: var(--color-hover);
  }

  button:disabled {
    opacity: 0.5;
    cursor: default;
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

  .more {
    display: block;
    margin: var(--space-sm) auto;
  }
</style>
