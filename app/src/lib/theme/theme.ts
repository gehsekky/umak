import defaultTheme from "./themes/default.json";
import testTheme from "./themes/test.json";

export type TokenName =
  | "color-bg"
  | "color-surface"
  | "color-surface-raised"
  | "color-border"
  | "color-text"
  | "color-text-muted"
  | "color-accent"
  | "color-on-accent"
  | "color-hover"
  | "color-selected"
  | "color-danger"
  | "color-backdrop"
  | "color-visual-bg"
  | "color-visual-fill"
  | "color-visual-peak"
  | "color-visual-line"
  | "color-wave"
  | "color-wave-played"
  | "font-body"
  | "font-size-small"
  | "font-size-body"
  | "font-size-title"
  | "space-xs"
  | "space-sm"
  | "space-md"
  | "space-lg"
  | "radius"
  | "size-row"
  | "size-sidebar"
  | "size-queue"
  | "size-visual-dock"
  | "size-waveform"
  | "size-album-tile"
  | "size-album-caption"
  | "size-album-cover";

export type Theme = {
  name: string;
  tokens: Record<TokenName, string>;
};

const themes: Record<string, Theme> = {
  default: defaultTheme,
  test: testTheme,
};

export function selectTheme(name: string | undefined): Theme {
  return themes[name ?? "default"] ?? themes.default;
}

/** Exposes the tokens of a theme as CSS variables on the document root. */
export function applyTheme(theme: Theme): void {
  const root = document.documentElement.style;
  for (const [token, value] of Object.entries(theme.tokens)) {
    root.setProperty(`--${token}`, value);
  }
}
