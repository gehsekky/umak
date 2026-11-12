// How the Settings view finds, groups and shows settings.

import type { SettingDefinition } from "../api/generated/SettingDefinition";
import type { SettingValue } from "../api/generated/SettingValue";

export type Section = { name: string; settings: SettingDefinition[] };

/** The text a search looks through for a setting. */
function searchText(definition: SettingDefinition): string {
  const options =
    definition.kind.type === "choice" ? definition.kind.options.map((option) => option.label) : [];
  return [definition.section, definition.label, definition.help, ...options]
    .join(" ")
    .toLowerCase();
}

/**
 * The settings listed in the Settings view under their sections, in the order the core
 * declares them. With a search, only those holding every word of it are left.
 */
export function sections(definitions: SettingDefinition[], query: string): Section[] {
  const words = query.toLowerCase().split(/\s+/).filter(Boolean);
  const found: Section[] = [];
  for (const definition of definitions) {
    if (definition.hidden) continue;
    const text = searchText(definition);
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
  return kind.unit ? `${shown} ${kind.unit}` : shown;
}
