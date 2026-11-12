// The shortcuts in use, from the settings, for running actions and naming their keys.

import { settings } from "../settings/state.svelte";
import { shortcutActions, type ActionId } from "./registry";
import {
  currentShortcuts,
  keyLabel,
  readChanged,
  SHORTCUTS_KEY,
  type ChangedShortcuts,
} from "./shortcuts";

export function changedShortcuts(): ChangedShortcuts {
  return readChanged(settings.value(SHORTCUTS_KEY));
}

/** The key an action has now, if any. */
export function shortcutOf(id: ActionId): string | null {
  return currentShortcuts(shortcutActions(), changedShortcuts()).get(id) ?? null;
}

/** For a button's title, such as " (Ctrl+Up and Ctrl+Down)", or nothing when there are no keys. */
export function shortcutHint(...ids: ActionId[]): string {
  const keys = ids.flatMap((id) => {
    const key = shortcutOf(id);
    return key ? [keyLabel(key)] : [];
  });
  return keys.length > 0 ? ` (${keys.join(" and ")})` : "";
}
