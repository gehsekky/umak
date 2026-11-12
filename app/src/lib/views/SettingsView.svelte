<script lang="ts">
  // Every setting, in sections, with a search. The volume itself is on the player bar.
  import { runAction, shortcutActions } from "../actions/registry";
  import { actionGroup, SHORTCUTS_KEY } from "../actions/shortcuts";
  import type { GainCoverage } from "../api/generated/GainCoverage";
  import { playerClient } from "../player/client";
  import Equalizer from "../settings/Equalizer.svelte";
  import OutputDevice from "../settings/OutputDevice.svelte";
  import SettingRow from "../settings/SettingRow.svelte";
  import ShortcutList from "../settings/ShortcutList.svelte";
  import ThemePicker from "../settings/ThemePicker.svelte";
  import { changedKeys, sections } from "../settings/show";
  import { settings } from "../settings/state.svelte";
  import { app } from "../state/app.svelte";

  let query = $state("");
  /** The search finds the shortcuts setting by its actions too. */
  const more = {
    [SHORTCUTS_KEY]: shortcutActions()
      .map((action) => `${actionGroup(action.id)} ${action.label}`)
      .join(" "),
  };
  const shown = $derived(sections(settings.definitions, query, more));
  /** Every section in full, so resetting one covers what the search leaves out. */
  const whole = $derived(sections(settings.definitions, ""));

  function changedIn(name: string): string[] {
    const section = whole.find((candidate) => candidate.name === name);
    return section ? changedKeys(section.settings, settings.values) : [];
  }

  let coverage = $state<GainCoverage | null>(null);

  $effect(() => {
    void app.libraryVersion;
    playerClient.gainCoverage().then(
      (found) => (coverage = found),
      () => (coverage = null),
    );
  });

  const replayGain = $derived(settings.value("player.replayGain"));

  /** The tags the chosen mode reads first. A track lacking them uses the other kind. */
  const tagged = $derived.by(() => {
    if (!coverage) return 0;
    return replayGain === "album" ? coverage.withAlbumGain : coverage.withTrackGain;
  });

  /** Settings set elsewhere, such as the volume, are not listed, so their problems show here. */
  const unlistedProblems = $derived(
    settings.definitions.filter(
      (definition) => definition.hidden && settings.values[definition.key]?.problem,
    ),
  );

  /** Settings that do nothing while another is set a certain way. */
  const inactive = $derived<Record<string, boolean>>({
    "player.preampDb": replayGain === "off",
  });
</script>

{#snippet equalizer()}
  <Equalizer />
{/snippet}

{#snippet themePicker()}
  <ThemePicker id="setting-appearance.theme" />
{/snippet}

{#snippet outputDevice()}
  <OutputDevice id="setting-player.outputDevice" />
{/snippet}

{#snippet shortcutList()}
  <ShortcutList id="setting-{SHORTCUTS_KEY}" {query} />
{/snippet}

{#snippet gainCoverage()}
  {#if coverage && coverage.tracks > 0}
    <p class="note" data-testid="coverage">
      {coverage.withTrackGain.toLocaleString()} of {coverage.tracks.toLocaleString()} tracks have
      track values, and {coverage.withAlbumGain.toLocaleString()} have album values.
      {#if replayGain !== "off" && tagged < coverage.tracks}
        <strong>Tracks without values play at their full level, so they may be louder than tagged ones.</strong>
      {/if}
    </p>
  {/if}
{/snippet}

<div class="page">
  <div class="top">
    <h1>Settings</h1>
    <input
      class="search"
      type="search"
      placeholder="Search settings"
      aria-label="Search settings"
      bind:value={query}
      onkeydown={(event) => {
        if (event.key === "Escape") query = "";
      }}
    />
  </div>

  {#each unlistedProblems as definition (definition.key)}
    <p class="problem">
      {definition.label}: {settings.values[definition.key]?.problem}
      <button onclick={() => runAction("settings.reset", { keys: [definition.key] })}>Reset</button>
    </p>
  {/each}

  {#each shown as section (section.name)}
    {@const changed = changedIn(section.name)}
    <section aria-label={section.name}>
      <div class="section-head">
        <h2>{section.name}</h2>
        <button
          disabled={changed.length === 0}
          title={changed.length === 0
            ? "Every setting here is at its default"
            : `Put the ${changed.length === 1 ? "setting" : `${changed.length} settings`} changed here back to the default`}
          onclick={() => runAction("settings.reset", { keys: changed })}
        >
          Reset {section.name}
        </button>
      </div>
      {#each section.settings as definition (definition.key)}
        <SettingRow
          {definition}
          disabled={inactive[definition.key] ?? false}
          control={definition.key === "player.eq"
            ? equalizer
            : definition.key === "appearance.theme"
              ? themePicker
              : definition.key === SHORTCUTS_KEY
                ? shortcutList
                : definition.key === "player.outputDevice"
                  ? outputDevice
                  : undefined}
          note={definition.key === "player.replayGain" ? gainCoverage : undefined}
        />
      {/each}
    </section>
  {:else}
    <p class="note">
      {settings.definitions.length === 0 ? "Loading..." : `No settings match "${query.trim()}".`}
    </p>
  {/each}
</div>

<style>
  .page {
    padding: var(--space-md) var(--space-lg);
    overflow-y: auto;
    max-width: 40em;
  }

  .top {
    display: flex;
    align-items: center;
    gap: var(--space-md);
    margin-bottom: var(--space-md);
  }

  h1 {
    margin: 0;
    font-size: var(--font-size-title);
    font-weight: 600;
  }

  .search {
    flex: 1;
    min-width: 0;
    max-width: 20em;
    padding: var(--space-xs) var(--space-sm);
    background: var(--color-surface);
    color: var(--color-text);
    border: 1px solid var(--color-border);
    border-radius: var(--radius);
    font: inherit;
  }

  .search:focus-visible,
  button:focus-visible {
    outline: 1px solid var(--color-accent);
    outline-offset: 1px;
  }

  .section-head {
    display: flex;
    align-items: baseline;
    gap: var(--space-md);
    margin-bottom: var(--space-md);
    padding-bottom: var(--space-xs);
    border-bottom: 1px solid var(--color-border);
  }

  h2 {
    flex: 1;
    margin: 0;
    font-size: var(--font-size-body);
    font-weight: 600;
    color: var(--color-text-muted);
    text-transform: uppercase;
    letter-spacing: 0.05em;
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

  .note {
    margin: var(--space-sm) 0 0;
    color: var(--color-text-muted);
    font-size: var(--font-size-small);
  }

  .problem {
    display: flex;
    align-items: baseline;
    gap: var(--space-sm);
    margin: 0 0 var(--space-md);
    color: var(--color-danger);
    font-size: var(--font-size-small);
  }

  .problem button {
    flex: none;
  }

  strong {
    color: var(--color-text);
    font-weight: 400;
  }
</style>
