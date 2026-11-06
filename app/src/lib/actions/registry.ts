// Every UI action lives here with an ID, a label and a handler, so buttons, shortcuts
// and later the command palette and context menus all run the same thing.

import type { ReplayGainMode } from "../api/generated/ReplayGainMode";

/** Actions and the arguments they take. */
export type ActionMap = {
  "nav.library": void;
  "nav.folders": void;
  "nav.playlist": { id: string };
  "nav.settings": void;
  "nav.tagEdits": void;
  "nav.tagEditor": void;
  "nav.tagReview": { id: string };
  "library.addFolder": void;
  "library.rescan": void;
  "library.cancelScan": void;
  "library.focusSearch": void;
  "library.export": void;
  "library.chooseImport": void;
  "library.startImport": { path: string; roots: { id: string; path: string | null }[] };
  "tags.edit": { trackIds: string[] };
  "tags.preview": void;
  "tags.write": { id: string };
  "tags.discard": { id: string };
  "tags.backToDraft": { id: string };
  "tags.closeDraft": void;
  "tags.leaveReview": void;
  "tags.undo": { id: string };
  "playlists.startCreate": void;
  "playlists.create": { name: string };
  "playlists.rename": { id: string; name: string };
  "playlists.delete": { id: string };
  "playlists.addTracks": { id: string; trackIds: string[] };
  "playlists.removeItems": { id: string; positions: number[] };
  "playlists.moveItems": { id: string; positions: number[]; before: number | null };
  "player.playTracks": { trackIds: string[]; startIndex: number };
  "player.playPlaylist": { id: string; startIndex: number };
  "player.toggle": void;
  "player.stop": void;
  "player.next": void;
  "player.previous": void;
  "player.seek": { ms: number };
  "player.setVolume": { volume: number };
  "player.toggleMute": void;
  "player.volumeUp": void;
  "player.volumeDown": void;
  "player.skipForward": void;
  "player.skipBack": void;
  "player.playAlbum": { trackId: string };
  "player.setGain": { replayGain: ReplayGainMode; preampDb: number };
  "player.setEq": { enabled: boolean; bands: number[] };
  "player.saveEqPreset": { name: string; bands: number[] };
  "player.deleteEqPreset": { name: string };
  "queue.togglePanel": void;
  "queue.add": { trackIds: string[] };
  "queue.playNext": { trackIds: string[] };
  "queue.play": { index: number };
  "queue.remove": { positions: number[] };
  "queue.move": { positions: number[]; before: number | null };
  "queue.clear": void;
  "queue.save": { name: string };
};

export type ActionId = keyof ActionMap;

export type Action<K extends ActionId> = {
  id: K;
  label: string;
  /** For example "ctrl+n". Actions with arguments cannot have one. */
  shortcut?: ActionMap[K] extends void ? string : never;
  run: (args: ActionMap[K]) => unknown;
};

type Errors = (message: string) => void;

// The stored handlers take their arguments as checked by `Action` when registered.
const actions = new Map<string, Action<ActionId>>();
let reportError: Errors = () => {};

export function registerAction<K extends ActionId>(action: Action<K>): void {
  actions.set(action.id, action as unknown as Action<ActionId>);
}

/** Sets where failures of actions are shown. */
export function onActionError(handler: Errors): void {
  reportError = handler;
}

export function actionLabel(id: ActionId): string {
  return actions.get(id)?.label ?? id;
}

/** Runs an action. Failures are reported, never thrown at the button that ran it. */
export async function runAction<K extends ActionId>(
  id: K,
  ...args: ActionMap[K] extends void ? [] : [ActionMap[K]]
): Promise<boolean> {
  const action = actions.get(id);
  if (!action) {
    reportError(`Unknown action: ${id}`);
    return false;
  }
  try {
    await (action.run as (args: unknown) => unknown)(args[0]);
    return true;
  } catch (failure) {
    reportError((failure as { message?: string }).message ?? String(failure));
    return false;
  }
}

/** Describes a key press the way shortcuts are written, such as "ctrl+shift+n". */
export function shortcutName(event: {
  key: string;
  ctrlKey: boolean;
  metaKey: boolean;
  shiftKey: boolean;
  altKey: boolean;
}): string {
  const parts: string[] = [];
  if (event.ctrlKey || event.metaKey) parts.push("ctrl");
  if (event.altKey) parts.push("alt");
  if (event.shiftKey) parts.push("shift");
  parts.push(event.key.toLowerCase());
  return parts.join("+");
}

/**
 * Whether a key press belongs to the element it happened in, not to a shortcut. Plain
 * keys belong to whatever is being typed in or pressed. Arrows with a modifier move
 * through text and move sliders, so they belong to those elements too.
 */
export function keyBelongsToElement(name: string, tag: string | null): boolean {
  if (!name.includes("+")) {
    return tag !== null && ["INPUT", "TEXTAREA", "SELECT", "BUTTON"].includes(tag);
  }
  return name.includes("arrow") && tag !== null && ["INPUT", "TEXTAREA", "SELECT"].includes(tag);
}

/** Runs the action bound to a key press, returning whether there was one. */
export function runShortcut(name: string): boolean {
  for (const action of actions.values()) {
    if (action.shortcut === name) {
      void runAction(action.id as "nav.library");
      return true;
    }
  }
  return false;
}
