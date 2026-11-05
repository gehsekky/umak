// Every UI action lives here with an ID, a label and a handler, so buttons, shortcuts
// and later the command palette and context menus all run the same thing.

/** Actions and the arguments they take. */
export type ActionMap = {
  "nav.library": void;
  "nav.folders": void;
  "nav.playlist": { id: string };
  "library.addFolder": void;
  "library.rescan": void;
  "library.cancelScan": void;
  "library.focusSearch": void;
  "playlists.startCreate": void;
  "playlists.create": { name: string };
  "playlists.rename": { id: string; name: string };
  "playlists.delete": { id: string };
  "playlists.addTracks": { id: string; trackIds: string[] };
  "playlists.removeItems": { id: string; positions: number[] };
  "playlists.moveItems": { id: string; positions: number[]; before: number | null };
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
