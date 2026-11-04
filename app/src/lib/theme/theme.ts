import defaultTheme from "./themes/default.json";
import testTheme from "./themes/test.json";

export type TokenName =
  | "color-bg"
  | "color-text"
  | "color-text-muted"
  | "color-accent"
  | "font-body"
  | "font-size-body"
  | "space-sm"
  | "space-lg";

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
