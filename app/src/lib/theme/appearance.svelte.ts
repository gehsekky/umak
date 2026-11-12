// The tokens in use: the theme the settings choose, at the text size and density they choose.

import { untrack } from "svelte";
import { call } from "../api/client";
import type { UserThemes } from "../api/generated/UserThemes";
import { settings } from "../settings/state.svelte";
import { app } from "../state/app.svelte";
import {
  BUILT_IN_THEMES,
  DENSITY_SCALES,
  TEST_THEME,
  applyTokens,
  checkTheme,
  fileThemeName,
  isTokens,
  scaleTokens,
  type Theme,
  type Tokens,
} from "./theme";

/** The last tokens applied, so the next start shows them before the settings arrive. */
const APPLIED_KEY = "umak.appliedTokens";

export type UserTheme = {
  /** The theme setting's name for it. */
  id: string;
  file: string;
  theme: Theme | null;
  problems: string[];
};

function supports(property: string, value: string): boolean {
  return typeof CSS !== "undefined" && CSS.supports(property, value);
}

/** The tokens to show before the settings are read: the last ones applied, or the default. */
export function startingTokens(): Tokens {
  if (import.meta.env.VITE_UMAK_THEME === "test") return TEST_THEME.tokens;
  try {
    const kept: unknown = JSON.parse(localStorage.getItem(APPLIED_KEY) ?? "null");
    if (isTokens(kept)) return kept;
  } catch {
    // Storage may be unavailable; the default theme does.
  }
  return BUILT_IN_THEMES.default.tokens;
}

class Appearance {
  folder = $state<string | null>(null);
  folderError = $state<string | null>(null);
  /** The user's theme files, checked. Null until the folder has been read. */
  userThemes = $state.raw<UserTheme[] | null>(null);
  /** Why the chosen theme is not the one shown. */
  problem = $state<string | null>(null);
  /** Changes whenever tokens are applied, for code that reads token values. */
  version = $state(0);

  async loadUserThemes(): Promise<void> {
    const found: UserThemes = await call("appearance.themes", {});
    this.folder = found.folder;
    this.folderError = found.error;
    this.userThemes = found.files.map((file) => {
      const id = fileThemeName(file.file);
      if (file.text === null) {
        return { id, file: file.file, theme: null, problems: [`It could not be read: ${file.error}`] };
      }
      const checked = checkTheme(file.text, file.file, supports);
      return "theme" in checked
        ? { id, file: file.file, theme: checked.theme, problems: [] }
        : { id, file: file.file, theme: null, problems: checked.problems };
    });
  }

  /** The theme a setting names, or why it cannot be used. Undefined while files are unread. */
  private resolve(name: string): { theme: Theme; problem: string | null } | undefined {
    const builtIn = BUILT_IN_THEMES[name];
    if (builtIn) return { theme: builtIn, problem: null };
    if (this.userThemes === null) return undefined;
    const fallback = BUILT_IN_THEMES.default;
    const found = this.userThemes.find((theme) => theme.id === name);
    if (!found) {
      const file = name.replace(/^file:/, "");
      return {
        theme: fallback,
        problem: `The theme ${file} is not in the themes folder, so the default theme is used.`,
      };
    }
    if (!found.theme) {
      return {
        theme: fallback,
        problem: `The theme ${found.file} has problems, so the default theme is used. ${found.problems[0]}`,
      };
    }
    return { theme: found.theme, problem: null };
  }

  private apply(): void {
    if (settings.definitions.length === 0) return;
    const name = settings.value("appearance.theme");
    const resolved =
      import.meta.env.VITE_UMAK_THEME === "test"
        ? { theme: TEST_THEME, problem: null }
        : this.resolve(typeof name === "string" ? name : "default");
    if (!resolved) return;
    const text = Number(settings.value("appearance.textSize") ?? 100) / 100;
    const density = DENSITY_SCALES[String(settings.value("appearance.density"))] ?? 1;
    const tokens = scaleTokens(resolved.theme.tokens, text, density);
    applyTokens(tokens);
    try {
      localStorage.setItem(APPLIED_KEY, JSON.stringify(tokens));
    } catch {
      // Only the next start's first moment depends on it.
    }
    untrack(() => {
      if (resolved.problem && resolved.problem !== this.problem) app.fail(resolved.problem);
      this.problem = resolved.problem;
      this.version++;
    });
  }

  /** Follows the settings from now on. */
  start(): () => void {
    void this.loadUserThemes().catch((failure: { message?: string }) => {
      this.userThemes = [];
      this.folderError = failure.message ?? String(failure);
    });
    return $effect.root(() => {
      $effect(() => this.apply());
    });
  }
}

export const appearance = new Appearance();
