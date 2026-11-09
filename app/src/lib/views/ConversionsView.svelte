<script lang="ts">
  import { untrack } from "svelte";
  import { actionLabel, runAction } from "../actions/registry";
  import { call } from "../api/client";
  import type { ConversionInfo } from "../api/generated/ConversionInfo";
  import type { ConvertFile } from "../api/generated/ConvertFile";
  import type { ConvertFileState } from "../api/generated/ConvertFileState";
  import { describeReport, describeSettings, needsAttention, retryable } from "../convert/options";
  import { countOf } from "../lists/format";
  import { app } from "../state/app.svelte";

  const PAGE = 100;
  const STATES: Record<ConvertFileState, string> = {
    pending: "Waiting",
    converted: "Converted",
    failed: "Failed",
    canceled: "Canceled",
    exists: "Already in the folder",
    sameOutput: "Shares an output name",
    missing: "Missing",
  };

  let conversions = $state.raw<ConversionInfo[]>([]);
  let loaded = $state(false);
  let loading = 0;
  /** The batch whose files are shown. */
  let opened = $state<string | null>(null);
  let files = $state.raw<ConvertFile[]>([]);
  let filesTotal = $state(0);
  let shown = $state(PAGE);
  let filesLoading = 0;

  async function load(): Promise<void> {
    const token = ++loading;
    const list = await call("convert.list", {});
    if (token !== loading) return;
    conversions = list.conversions;
    loaded = true;
  }

  async function loadFiles(jobId: string, limit: number): Promise<void> {
    const token = ++filesLoading;
    const page = await call("convert.files", { jobId, problemsOnly: true, offset: 0, limit });
    if (token !== filesLoading) return;
    files = page.files;
    filesTotal = page.total;
  }

  function report(failure: { message?: string }): void {
    app.fail(failure.message ?? String(failure));
  }

  $effect(() => {
    void app.conversionsVersion;
    untrack(() => {
      load().catch(report);
      if (opened) loadFiles(opened, shown).catch(report);
    });
  });

  // A batch starting or ending changes which one is running.
  $effect(() => {
    void app.conversion?.id;
    untrack(() => load().catch(report));
  });

  function toggle(jobId: string): void {
    if (opened === jobId) {
      opened = null;
      return;
    }
    opened = jobId;
    shown = PAGE;
    files = [];
    filesTotal = 0;
    loadFiles(jobId, PAGE).catch(report);
  }

  function showMore(jobId: string): void {
    shown += PAGE;
    loadFiles(jobId, shown).catch(report);
  }

  function running(batch: ConversionInfo): boolean {
    return batch.status === "running";
  }

  function percent(done: number, total: number): number {
    return total > 0 ? Math.floor((done * 100) / total) : 0;
  }

  function status(batch: ConversionInfo): string {
    if (batch.status === "canceled") return "Canceled";
    if (batch.status === "failed") return `Failed: ${batch.error ?? "unknown error"}`;
    return "";
  }
</script>

<div class="page">
  <h1>{actionLabel("nav.conversions")}</h1>

  {#if loaded && conversions.length === 0}
    <p class="meta">
      No conversions yet. To convert, select tracks in the library or a playlist and choose
      {actionLabel("convert.open")}.
    </p>
  {/if}

  {#each conversions as batch (batch.jobId)}
    {@const live = running(batch) && app.conversion?.id === batch.jobId ? app.conversion : null}
    <div class="batch" class:attention={!running(batch) && needsAttention(batch.report)}>
      <div class="row">
        <div class="details">
          <div class="name">{describeSettings(batch.settings)}</div>
          <div class="meta" title={batch.folder}>To {batch.folder}</div>
          <div class="meta">
            {new Date(batch.finishedAt ?? batch.createdAt).toLocaleString()}
            {#if status(batch)}· {status(batch)}{/if}
            {#if !running(batch)}· {describeReport(batch.report)}{/if}
          </div>
          {#if running(batch)}
            <div class="progress">
              {#if live}
                <span class="meta">
                  {live.done.toLocaleString()} of {live.total.toLocaleString()} files
                </span>
                <span class="meter" aria-hidden="true">
                  <span class="fill" style:width="{percent(live.done, live.total)}%"></span>
                </span>
              {:else}
                <span class="meta">Starting...</span>
              {/if}
            </div>
          {/if}
        </div>
        <button onclick={() => toggle(batch.jobId)} aria-expanded={opened === batch.jobId}>
          {opened === batch.jobId ? "Hide files" : "Files"}
        </button>
        {#if running(batch)}
          <button onclick={() => runAction("convert.cancel", { jobId: batch.jobId })}>
            {actionLabel("convert.cancel")}
          </button>
        {:else}
          <button
            disabled={retryable(batch.report) === 0 || app.conversion !== null}
            title={retryable(batch.report) > 0
              ? `Convert the ${countOf(retryable(batch.report), "file")} that did not convert again`
              : "Every file was dealt with"}
            onclick={() => runAction("convert.retry", { jobId: batch.jobId })}
          >
            {actionLabel("convert.retry")}
          </button>
        {/if}
      </div>

      {#if opened === batch.jobId}
        <div class="files">
          {#if filesTotal === 0}
            <p class="meta">
              {running(batch)
                ? "No problems so far."
                : "Every file converted with all of its tags, or was already in the folder."}
            </p>
          {:else}
            <p class="meta">
              {countOf(filesTotal, "file")} not converted, or converted without every tag:
            </p>
            {#each files as file, index (index)}
              <div class="file">
                <span class="state" class:failed={file.state === "failed"}>{STATES[file.state]}</span>
                <div class="details">
                  <div class="path" title={file.outputPath}>{file.sourcePath}</div>
                  {#if file.message}
                    <div class="meta message">{file.message}</div>
                  {/if}
                  {#if file.leftOut.length > 0}
                    <div class="meta">Left out: {file.leftOut.join(", ")}</div>
                  {/if}
                </div>
              </div>
            {/each}
            {#if files.length < filesTotal}
              <button class="more" onclick={() => showMore(batch.jobId)}>
                Show more ({(filesTotal - files.length).toLocaleString()} left)
              </button>
            {/if}
          {/if}
        </div>
      {/if}
    </div>
  {/each}
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

  .batch {
    padding: var(--space-sm) var(--space-md);
    margin-bottom: var(--space-sm);
    background: var(--color-surface);
    border: 1px solid var(--color-border);
    border-radius: var(--radius);
  }

  .batch.attention {
    border-color: var(--color-danger);
  }

  .row {
    display: flex;
    align-items: center;
    gap: var(--space-sm);
  }

  .details {
    flex: 1;
    min-width: 0;
  }

  .name {
    font-weight: 600;
  }

  .progress {
    display: flex;
    align-items: center;
    gap: var(--space-sm);
    margin-top: var(--space-xs);
  }

  .meter {
    width: 16em;
    max-width: 50%;
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

  .files {
    margin-top: var(--space-sm);
    padding-top: var(--space-sm);
    border-top: 1px solid var(--color-border);
  }

  .file {
    display: flex;
    gap: var(--space-sm);
    padding: var(--space-xs) 0;
  }

  .state {
    flex: none;
    width: 11em;
    color: var(--color-text-muted);
    font-size: var(--font-size-small);
  }

  .state.failed {
    color: var(--color-danger);
  }

  .path {
    overflow-wrap: anywhere;
  }

  .message {
    white-space: pre-wrap;
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
    margin: var(--space-sm) auto 0;
  }
</style>
