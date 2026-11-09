<script lang="ts">
  import { actionLabel, runAction } from "../actions/registry";
  import { app, type JobState } from "../state/app.svelte";

  const NOTICE_MS = 8000;

  // Errors stay until dismissed, so they cannot be missed.
  $effect(() => {
    if (app.notice && !app.notice.error) {
      const timer = setTimeout(() => (app.notice = null), NOTICE_MS);
      return () => clearTimeout(timer);
    }
  });

  const JOB_LABELS: Record<string, string> = {
    scan: "Scanning",
    import: "Importing",
    tagPreview: "Reading tags",
    tagWrite: "Writing tags",
  };

  function percent(job: JobState): number {
    return job.total > 0 ? Math.floor((job.done * 100) / job.total) : 0;
  }
</script>

<div class="bar" role="status">
  {#if app.scan}
    <span class="job">
      {JOB_LABELS[app.scan.kind]}:
      {app.scan.done.toLocaleString()} of {app.scan.total.toLocaleString()} files
    </span>
    <span class="meter" aria-hidden="true">
      <span class="fill" style:width="{percent(app.scan)}%"></span>
    </span>
    <button onclick={() => runAction("library.cancelScan")}>
      {actionLabel("library.cancelScan")}
    </button>
  {/if}
  {#if app.conversion}
    {@const conversion = app.conversion}
    <button class="job" title="Show conversions" onclick={() => runAction("nav.conversions")}>
      Converting: {conversion.done.toLocaleString()} of {conversion.total.toLocaleString()} files
    </button>
    <span class="meter" aria-hidden="true">
      <span class="fill" style:width="{percent(conversion)}%"></span>
    </span>
    <button onclick={() => runAction("convert.cancel", { jobId: conversion.id })}>
      {actionLabel("convert.cancel")}
    </button>
  {/if}
  {#if app.notice && !(app.scan && app.conversion)}
    <span class="notice" class:error={app.notice.error}>{app.notice.text}</span>
    <button class="dismiss" aria-label="Dismiss" onclick={() => (app.notice = null)}>×</button>
  {:else if !app.scan && !app.conversion}
    <span>&nbsp;</span>
  {/if}
</div>

<style>
  .bar {
    display: flex;
    align-items: center;
    gap: var(--space-sm);
    padding: var(--space-xs) var(--space-md);
    color: var(--color-text-muted);
    font-size: var(--font-size-small);
    min-height: 1.6em;
  }

  .error {
    color: var(--color-danger);
  }

  .job {
    flex: none;
  }

  .notice {
    min-width: 0;
    overflow: hidden;
    white-space: nowrap;
    text-overflow: ellipsis;
  }

  .meter + button {
    margin-right: var(--space-md);
  }

  .meter {
    width: 12em;
    height: var(--space-xs);
    background: var(--color-surface-raised);
    border-radius: var(--radius);
    overflow: hidden;
  }

  .fill {
    display: block;
    height: 100%;
    background: var(--color-accent);
  }

  button {
    all: unset;
    cursor: pointer;
    padding: 0 var(--space-xs);
    border-radius: var(--radius);
  }

  button:hover {
    color: var(--color-text);
    background: var(--color-hover);
  }

  button:focus-visible {
    outline: 1px solid var(--color-accent);
  }
</style>
