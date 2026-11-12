<script lang="ts">
  // One setting: its control, what it does, and whether it differs from its default.
  import type { Snippet } from "svelte";
  import { runAction } from "../actions/registry";
  import type { SettingDefinition } from "../api/generated/SettingDefinition";
  import { latestOnly } from "../player/volume";
  import OrderList from "./OrderList.svelte";
  import TextList from "./TextList.svelte";
  import { formatNumber } from "./show";
  import { settings } from "./state.svelte";

  type Props = {
    definition: SettingDefinition;
    /** For a setting with a control of its own. */
    control?: Snippet;
    /** Shown under the help, such as facts about the library. */
    note?: Snippet;
    disabled?: boolean;
  };

  let { definition, control, note, disabled = false }: Props = $props();

  const id = $derived(`setting-${definition.key}`);
  const current = $derived(settings.values[definition.key]);
  const value = $derived(current?.value ?? definition.default);
  const changed = $derived(current?.isDefault === false);

  const send = latestOnly((value: unknown) =>
    runAction("settings.set", { key: definition.key, value }),
  );
  // While a slider is held, it follows the pointer.
  let draft = $state<number | null>(null);
</script>

<div class="setting" class:disabled data-testid={id}>
  <div class="head">
    {#if definition.kind.type === "toggle"}
      <label class="check">
        <input
          {id}
          type="checkbox"
          checked={value === true}
          {disabled}
          onchange={(event) => send(event.currentTarget.checked)}
        />
        {definition.label}
      </label>
    {:else if ["custom", "order", "several", "list"].includes(definition.kind.type)}
      <span class="label">{definition.label}</span>
    {:else}
      <label class="label" for={id}>{definition.label}</label>
    {/if}
    {#if changed}
      <span class="changed" title="Changed from the default">Changed</span>
      <button
        class="reset"
        aria-label="Reset {definition.label} to its default"
        onclick={() => runAction("settings.reset", { keys: [definition.key] })}
      >
        Reset
      </button>
    {/if}
  </div>

  {#if definition.kind.type === "number"}
    {@const kind = definition.kind}
    {@const shown = draft ?? (typeof value === "number" ? value : kind.min)}
    <div class="slider">
      <input
        {id}
        type="range"
        min={kind.min}
        max={kind.max}
        step={kind.step}
        value={shown}
        {disabled}
        oninput={(event) => {
          draft = event.currentTarget.valueAsNumber;
          send(draft);
        }}
        onchange={() => (draft = null)}
      />
      <output for={id}>{formatNumber(shown, kind)}</output>
    </div>
  {:else if definition.kind.type === "choice"}
    <select {id} value={value} {disabled} onchange={(event) => send(event.currentTarget.value)}>
      {#each definition.kind.options as option (option.value)}
        <option value={option.value}>{option.label}</option>
      {/each}
    </select>
  {:else if definition.kind.type === "order"}
    <OrderList
      {id}
      options={definition.kind.options}
      value={Array.isArray(value) ? value.filter((name) => typeof name === "string") : []}
      {disabled}
      onchange={send}
    />
  {:else if definition.kind.type === "several"}
    {@const chosen = Array.isArray(value) ? value : []}
    <div class="several" {id}>
      {#each definition.kind.options as option (option.value)}
        <label class="option">
          <input
            type="checkbox"
            checked={chosen.includes(option.value)}
            {disabled}
            onchange={(event) =>
              send(
                event.currentTarget.checked
                  ? [...chosen, option.value]
                  : chosen.filter((name) => name !== option.value),
              )}
          />
          {option.label}
        </label>
      {/each}
    </div>
  {:else if definition.kind.type === "list"}
    <TextList
      {id}
      label={definition.label}
      value={Array.isArray(value) ? value.filter((item) => typeof item === "string") : []}
      {disabled}
      onchange={send}
    />
  {:else if definition.kind.type === "custom"}
    {@render control?.()}
  {/if}

  {#if definition.help}
    <p class="help">{definition.help}</p>
  {/if}
  {@render note?.()}
  {#if current?.problem}
    <p class="problem">{current.problem}</p>
  {/if}
</div>

<style>
  .setting {
    margin-bottom: var(--space-lg);
  }

  .head {
    display: flex;
    align-items: baseline;
    gap: var(--space-sm);
    margin-bottom: var(--space-xs);
  }

  .label,
  .check {
    font-weight: 600;
  }

  .check {
    display: flex;
    align-items: center;
    gap: var(--space-xs);
    cursor: pointer;
  }

  .changed {
    color: var(--color-accent);
    font-size: var(--font-size-small);
  }

  .reset {
    padding: 0;
    background: none;
    border: none;
    color: var(--color-text-muted);
    font: inherit;
    font-size: var(--font-size-small);
    text-decoration: underline;
    cursor: pointer;
  }

  .reset:hover {
    color: var(--color-text);
  }

  .reset:focus-visible,
  select:focus-visible,
  input:focus-visible {
    outline: 1px solid var(--color-accent);
    outline-offset: 1px;
  }

  select {
    padding: var(--space-xs) var(--space-sm);
    background: var(--color-surface-raised);
    color: var(--color-text);
    border: 1px solid var(--color-border);
    border-radius: var(--radius);
    font: inherit;
  }

  .several {
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(6em, 1fr));
    gap: var(--space-xs) var(--space-sm);
    max-width: 32em;
  }

  .option {
    display: flex;
    align-items: center;
    gap: var(--space-xs);
    cursor: pointer;
  }

  .slider {
    display: flex;
    align-items: center;
    gap: var(--space-sm);
  }

  input {
    margin: 0;
    accent-color: var(--color-accent);
  }

  .slider input {
    flex: 1;
    max-width: 24em;
  }

  output {
    color: var(--color-text-muted);
    font-size: var(--font-size-small);
    font-variant-numeric: tabular-nums;
    white-space: nowrap;
  }

  .help,
  .problem {
    margin: var(--space-sm) 0 0;
    font-size: var(--font-size-small);
  }

  .help {
    color: var(--color-text-muted);
  }

  .problem {
    color: var(--color-danger);
  }

  .disabled .slider,
  .disabled select {
    opacity: 0.5;
  }
</style>
