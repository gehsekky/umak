<script lang="ts">
  // The theme setting's control: the themes that come with the app and the user's own files,
  // with what is wrong with any file that cannot be used.
  import { onMount } from "svelte";
  import { runAction } from "../actions/registry";
  import { appearance } from "../theme/appearance.svelte";
  import { BUILT_IN_THEMES } from "../theme/theme";
  import { settings } from "./state.svelte";
  import { app } from "../state/app.svelte";

  let { id }: { id: string } = $props();

  const chosen = $derived(String(settings.value("appearance.theme") ?? "default"));
  const userThemes = $derived(appearance.userThemes ?? []);
  const missing = $derived(
    !BUILT_IN_THEMES[chosen] &&
      appearance.userThemes !== null &&
      !userThemes.some((theme) => theme.id === chosen),
  );
  let reloading = $state(false);

  onMount(() => {
    // Files may have been added or edited since the app started.
    void reload();
  });

  async function reload(): Promise<void> {
    reloading = true;
    try {
      await appearance.loadUserThemes();
    } catch (failure) {
      app.fail((failure as { message?: string }).message ?? String(failure));
    } finally {
      reloading = false;
    }
  }

  async function copy(text: string, what: string): Promise<void> {
    try {
      await navigator.clipboard.writeText(text);
      app.notify(`Copied ${what}`);
    } catch {
      app.fail(`Could not copy ${what}`);
    }
  }

  /** The chosen theme's own tokens, before text size and density, as a file to start from. */
  function currentAsFile(): string {
    const theme =
      BUILT_IN_THEMES[chosen] ??
      userThemes.find((found) => found.id === chosen)?.theme ??
      BUILT_IN_THEMES.default;
    return JSON.stringify({ name: `My ${theme.name}`, tokens: theme.tokens }, null, 2);
  }
</script>

<div class="row">
  <select
    {id}
    value={chosen}
    onchange={(event) =>
      runAction("settings.set", { key: "appearance.theme", value: event.currentTarget.value })}
  >
    <optgroup label="Come with the app">
      {#each Object.entries(BUILT_IN_THEMES) as [name, theme] (name)}
        <option value={name}>{theme.name}</option>
      {/each}
    </optgroup>
    {#if userThemes.length > 0 || missing}
      <optgroup label="Your themes">
        {#each userThemes as theme (theme.id)}
          <option value={theme.id} disabled={theme.theme === null}>
            {theme.theme?.name ?? theme.file}{theme.theme ? "" : " (has problems)"}
          </option>
        {/each}
        {#if missing}
          <option value={chosen}>{chosen.replace(/^file:/, "")} (missing)</option>
        {/if}
      </optgroup>
    {/if}
  </select>
  <button onclick={reload} disabled={reloading}>Reload themes</button>
  <button onclick={() => copy(currentAsFile(), "this theme as a file to start from")}>
    Copy as a theme file
  </button>
</div>

{#if appearance.folder}
  <p class="folder">
    Themes folder: <code>{appearance.folder}</code>
    <button class="link" onclick={() => copy(appearance.folder ?? "", "the folder's path")}>
      Copy path
    </button>
  </p>
{/if}
{#if appearance.folderError}
  <p class="problem">The themes folder could not be read: {appearance.folderError}</p>
{/if}
{#if appearance.problem}
  <p class="problem">{appearance.problem}</p>
{/if}
{#each userThemes.filter((theme) => theme.problems.length > 0) as theme (theme.id)}
  <div class="problem">
    {theme.file}:
    <ul>
      {#each theme.problems as problem, index (index)}
        <li>{problem}</li>
      {/each}
    </ul>
  </div>
{/each}

<style>
  .row {
    display: flex;
    align-items: center;
    gap: var(--space-xs);
    flex-wrap: wrap;
  }

  select {
    padding: var(--space-xs) var(--space-sm);
    background: var(--color-surface-raised);
    color: var(--color-text);
    border: 1px solid var(--color-border);
    border-radius: var(--radius);
    font: inherit;
  }

  button {
    padding: var(--space-xs) var(--space-sm);
    background: var(--color-surface-raised);
    color: var(--color-text);
    border: 1px solid var(--color-border);
    border-radius: var(--radius);
    font: inherit;
    font-size: var(--font-size-small);
    cursor: pointer;
  }

  button:hover:enabled {
    background: var(--color-hover);
  }

  button:disabled {
    color: var(--color-text-muted);
    cursor: default;
  }

  button.link {
    padding: 0;
    background: none;
    border: none;
    color: var(--color-text-muted);
    text-decoration: underline;
  }

  button.link:hover {
    color: var(--color-text);
  }

  select:focus-visible,
  button:focus-visible {
    outline: 1px solid var(--color-accent);
    outline-offset: 1px;
  }

  .folder,
  .problem {
    margin: var(--space-sm) 0 0;
    font-size: var(--font-size-small);
  }

  .folder {
    color: var(--color-text-muted);
    overflow-wrap: anywhere;
  }

  code {
    color: var(--color-text);
  }

  .problem {
    color: var(--color-danger);
  }

  ul {
    margin: var(--space-xs) 0 0;
    padding-left: var(--space-lg);
  }
</style>
