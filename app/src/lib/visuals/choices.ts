// Which visualizer each place shows, remembered between runs.

export type VisualPlace = "dock" | "full";
export type VisualChoices = Record<VisualPlace, string | null>;

const KEY = "umak.visuals";

export function parseChoices(saved: string | null): VisualChoices {
  const choices: VisualChoices = { dock: null, full: null };
  if (saved === null) return choices;
  try {
    const parsed: unknown = JSON.parse(saved);
    if (typeof parsed !== "object" || parsed === null) return choices;
    for (const place of ["dock", "full"] as const) {
      const id = (parsed as Record<string, unknown>)[place];
      if (typeof id === "string") choices[place] = id;
    }
  } catch {
    // A damaged entry is forgotten.
  }
  return choices;
}

export function loadChoices(): VisualChoices {
  try {
    return parseChoices(localStorage.getItem(KEY));
  } catch {
    return parseChoices(null);
  }
}

export function saveChoices(choices: VisualChoices): void {
  try {
    localStorage.setItem(KEY, JSON.stringify(choices));
  } catch {
    // Remembering this is only a convenience.
  }
}
