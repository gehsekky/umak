// How the Settings view finds, groups and shows settings.

import type { SettingDefinition } from "../api/generated/SettingDefinition";
import type { SettingOption } from "../api/generated/SettingOption";
import type { SettingValue } from "../api/generated/SettingValue";

export type Section = { name: string; settings: SettingDefinition[] };

/** The text a search looks through for a setting, with any more its control lists. */
function searchText(definition: SettingDefinition, more: string): string {
  const kind = definition.kind;
  const options =
    kind.type === "choice" || kind.type === "order" || kind.type === "several"
      ? kind.options.map((option) => option.label)
      : [];
  return [definition.section, definition.label, definition.help, ...options, more]
    .join(" ")
    .toLowerCase();
}

/**
 * The settings listed in the Settings view under their sections, in the order the core
 * declares them. With a search, only those holding every word of it are left. A setting with
 * a control of its own, such as a list of shortcuts, can give more text to search.
 */
export function sections(
  definitions: SettingDefinition[],
  query: string,
  more: Record<string, string> = {},
): Section[] {
  const words = query.toLowerCase().split(/\s+/).filter(Boolean);
  const found: Section[] = [];
  for (const definition of definitions) {
    if (definition.hidden) continue;
    const text = searchText(definition, more[definition.key] ?? "");
    if (!words.every((word) => text.includes(word))) continue;
    let section = found.find((candidate) => candidate.name === definition.section);
    if (!section) {
      section = { name: definition.section, settings: [] };
      found.push(section);
    }
    section.settings.push(definition);
  }
  return found;
}

/** The settings in a section whose values differ from their defaults. */
export function changedKeys(
  section: SettingDefinition[],
  values: Record<string, SettingValue>,
): string[] {
  return section
    .filter((definition) => values[definition.key]?.isDefault === false)
    .map((definition) => definition.key);
}

/** A number as a setting shows it, with as many decimals as its step and a sign if it can go below zero. */
export function formatNumber(
  value: number,
  kind: { min: number; step: number; unit: string | null },
): string {
  const decimals = Math.min(3, String(kind.step).split(".")[1]?.length ?? 0);
  const sign = kind.min < 0 && value > 0 ? "+" : "";
  const shown = `${sign}${value.toFixed(decimals)}`;
  if (!kind.unit) return shown;
  return kind.unit === "%" ? `${shown}%` : `${shown} ${kind.unit}`;
}

/** An order setting's options: the chosen ones in their order, then the rest. */
export function orderedOptions(options: SettingOption[], chosen: string[]): SettingOption[] {
  const picked = chosen.flatMap((name) => options.filter((option) => option.value === name));
  return [...picked, ...options.filter((option) => !chosen.includes(option.value))];
}

/** A list with one entry moved a place up (-1) or down (1). */
export function moveOption(chosen: string[], place: number, by: -1 | 1): string[] {
  const target = place + by;
  if (target < 0 || target >= chosen.length) return chosen;
  const moved = [...chosen];
  [moved[place], moved[target]] = [moved[target], moved[place]];
  return moved;
}
