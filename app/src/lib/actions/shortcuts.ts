// Which key runs which action. Each action has a shortcut by default; the user's changes are
// kept in one setting, by action ID, as a key or null for none.

import type { ActionId, ShortcutAction } from "./registry";

export const SHORTCUTS_KEY = "keys.shortcuts";

export type ChangedShortcuts = Record<string, string | null>;

/** The changes held by the setting's value, leaving out anything that is not a key or none. */
export function readChanged(value: unknown): ChangedShortcuts {
  if (typeof value !== "object" || value === null || Array.isArray(value)) return {};
  const changed: ChangedShortcuts = {};
  for (const [id, key] of Object.entries(value)) {
    if (key === null || (typeof key === "string" && key !== "")) changed[id] = key;
  }
  return changed;
}

/**
 * Each action's shortcut. A key the user gave an action is taken from any action that has it
 * by default, such as one given that key by a later version.
 */
export function currentShortcuts(
  actions: ShortcutAction[],
  changed: ChangedShortcuts,
): Map<ActionId, string | null> {
  const given = new Set(
    actions.flatMap((action) => {
      const key = changed[action.id];
      return action.id in changed && key ? [key] : [];
    }),
  );
  const current = new Map<ActionId, string | null>();
  for (const action of actions) {
    if (action.id in changed) {
      current.set(action.id, changed[action.id]);
    } else {
      current.set(action.id, action.shortcut && given.has(action.shortcut) ? null : action.shortcut);
    }
  }
  return current;
}

/** The action a key runs, if any. */
export function actionForKey(
  key: string,
  actions: ShortcutAction[],
  changed: ChangedShortcuts,
): ActionId | null {
  for (const [id, shortcut] of currentShortcuts(actions, changed)) {
    if (shortcut === key) return id;
  }
  return null;
}

/**
 * Gives an action a key, or none, taking the key from any action that has it. A key that is
 * an action's default is kept as no change at all.
 */
export function assignShortcut(
  actions: ShortcutAction[],
  changed: ChangedShortcuts,
  id: ActionId,
  key: string | null,
): { changed: ChangedShortcuts; takenFrom: ActionId[] } {
  const next = { ...changed };
  const takenFrom: ActionId[] = [];
  const current = currentShortcuts(actions, changed);
  for (const action of actions) {
    if (action.id === id || key === null || current.get(action.id) !== key) continue;
    takenFrom.push(action.id);
    if (action.shortcut === null) delete next[action.id];
    else next[action.id] = null;
  }
  const action = actions.find((candidate) => candidate.id === id);
  if (action && action.shortcut === key) delete next[id];
  else next[id] = key;
  return { changed: next, takenFrom };
}

const KEY_NAMES: Record<string, string> = {
  " ": "Space",
  arrowleft: "Left",
  arrowright: "Right",
  arrowup: "Up",
  arrowdown: "Down",
  escape: "Esc",
  pageup: "Page Up",
  pagedown: "Page Down",
};

const MODIFIERS: [string, string][] = [
  ["ctrl+", "Ctrl"],
  ["alt+", "Alt"],
  ["shift+", "Shift"],
];

/** A shortcut as people write it, such as "Ctrl+Shift+N" or "Space". */
export function keyLabel(key: string): string {
  const parts: string[] = [];
  let rest = key;
  for (const [prefix, name] of MODIFIERS) {
    // The key itself may be "+", as in "ctrl++".
    if (rest.startsWith(prefix) && rest.length > prefix.length) {
      parts.push(name);
      rest = rest.slice(prefix.length);
    }
  }
  const named = KEY_NAMES[rest];
  parts.push(named ?? (rest.length === 1 ? rest.toUpperCase() : rest[0].toUpperCase() + rest.slice(1)));
  return parts.join("+");
}

/** Keys that cannot be a shortcut on their own, because they only change others. */
const MODIFIER_KEYS = new Set(["control", "shift", "alt", "meta", "altgraph", "os"]);

/**
 * Why a key press cannot be a shortcut, or null if it can. An empty reason is a modifier
 * pressed on its own, the start of a shortcut still being pressed.
 */
export function unusableKey(key: string): string | null {
  const last = key.slice(key.lastIndexOf("+", key.length - 2) + 1);
  if (MODIFIER_KEYS.has(last)) return "";
  if (last === "tab") return "Tab moves between controls, so it cannot be a shortcut.";
  if (last === "dead" || last === "unidentified" || last === "process") {
    return "That key does not say which it is, so it cannot be a shortcut.";
  }
  return null;
}

const GROUPS: Record<string, string> = {
  nav: "Go to",
  library: "Library",
  playlists: "Playlists",
  player: "Playback",
  queue: "Queue",
  visuals: "Visualizer",
  tags: "Tags",
};

/** The heading an action is listed under, from the first part of its ID. */
export function actionGroup(id: ActionId): string {
  return GROUPS[id.split(".")[0]] ?? "Other";
}
