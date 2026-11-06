<script lang="ts">
  import { actionLabel, runAction } from "../actions/registry";
  import { app } from "../state/app.svelte";

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

  const percent = $derived(
    app.scan && app.scan.total > 0 ? Math.floor((app.scan.done * 100) / app.scan.total) : 0,
  );
</script>

<div class="bar" role="status">
  {#if app.scan}
    <span>
      {JOB_LABELS[app.scan.kind]}:
      {app.scan.done.toLocaleString()} of {app.scan.total.toLocaleString()} files
    </span>
    <span class="meter" aria-hidden="true"><span class="fill" style:width="{percent}%"></span></span>
    <button onclick={() => runAction("library.cancelScan")}>
      {actionLabel("library.cancelScan")}
    </button>
  {:else if app.notice}
    <span class:error={app.notice.error}>{app.notice.text}</span>
    <button class="dismiss" aria-label="Dismiss" onclick={() => (app.notice = null)}>×</button>
  {:else}
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
