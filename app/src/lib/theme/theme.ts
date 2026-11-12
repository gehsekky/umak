// Themes: named sets of tokens, which every color, font and size in the UI comes from.

import defaultTheme from "./themes/default.json";
import lightTheme from "./themes/light.json";
import testTheme from "./themes/test.json";

export const TOKEN_NAMES = [
  "color-bg",
  "color-surface",
  "color-surface-raised",
  "color-border",
  "color-text",
  "color-text-muted",
  "color-accent",
  "color-on-accent",
  "color-hover",
  "color-selected",
  "color-danger",
  "color-backdrop",
  "color-visual-bg",
  "color-visual-fill",
  "color-visual-peak",
  "color-visual-line",
  "color-wave",
  "color-wave-played",
  "font-body",
  "font-size-small",
  "font-size-body",
  "font-size-title",
  "space-xs",
  "space-sm",
  "space-md",
  "space-lg",
  "radius",
  "size-row",
  "size-sidebar",
  "size-queue",
  "size-visual-dock",
  "size-waveform",
  "size-album-tile",
  "size-album-caption",
  "size-album-cover",
  "size-player-art",
  "size-visual-art",
  "size-tag-picture",
] as const;

export type TokenName = (typeof TOKEN_NAMES)[number];
export type Tokens = Record<TokenName, string>;

export type Theme = {
  name: string;
  tokens: Tokens;
};

/** The themes that come with the app, by the name the theme setting holds. */
export const BUILT_IN_THEMES: Record<string, Theme> = {
  default: defaultTheme,
  light: lightTheme,
};

/** Exaggerated tokens for trying the UI, chosen only by `VITE_UMAK_THEME=test` at build time. */
export const TEST_THEME: Theme = testTheme;

/** The theme setting's name for a theme file in the themes folder. */
export function fileThemeName(file: string): string {
  return `file:${file}`;
}

type TokenKind = "color" | "font" | "size";

function tokenKind(name: TokenName): TokenKind {
  if (name.startsWith("color-")) return "color";
  if (name === "font-body") return "font";
  return "size";
}

function isTokenName(name: string): name is TokenName {
  return (TOKEN_NAMES as readonly string[]).includes(name);
}

/** Whether a CSS property takes a value, as `CSS.supports` says. */
export type Supports = (property: string, value: string) => boolean;

const PIXELS = /^\d+(\.\d+)?px$/;

/**
 * Checks a theme file. It holds a "tokens" object, and may give a "name" and a "base", one of
 * the themes that come with the app, which gives the tokens it leaves out.
 */
export function checkTheme(
  text: string,
  file: string,
  supports: Supports,
): { theme: Theme } | { problems: string[] } {
  let parsed: unknown;
  try {
    parsed = JSON.parse(text);
  } catch (failure) {
    return { problems: [`It is not valid JSON: ${(failure as Error).message}`] };
  }
  if (typeof parsed !== "object" || parsed === null || Array.isArray(parsed)) {
    return { problems: ["It needs to be a JSON object with a \"tokens\" object in it."] };
  }
  const { name, base, tokens, ...rest } = parsed as Record<string, unknown>;
  const problems: string[] = [];
  for (const key of Object.keys(rest)) {
    problems.push(`"${key}" is not part of a theme; it takes "name", "base" and "tokens".`);
  }
  if (name !== undefined && typeof name !== "string") problems.push('"name" needs to be text.');
  const baseName = base ?? "default";
  const baseTheme = typeof baseName === "string" ? BUILT_IN_THEMES[baseName] : undefined;
  if (!baseTheme) {
    problems.push(`"base" needs to be one of ${Object.keys(BUILT_IN_THEMES).join(", ")}.`);
  }
  if (typeof tokens !== "object" || tokens === null || Array.isArray(tokens)) {
    problems.push('It needs a "tokens" object.');
    return { problems };
  }
  const given: Partial<Tokens> = {};
  for (const [token, value] of Object.entries(tokens)) {
    if (!isTokenName(token)) {
      problems.push(`There is no token named "${token}".`);
    } else if (typeof value !== "string") {
      problems.push(`"${token}" needs to be text, such as "${defaultTheme.tokens[token]}".`);
    } else {
      const kind = tokenKind(token);
      if (kind === "size" && !PIXELS.test(value)) {
        problems.push(`"${token}" needs a size in pixels, such as "${defaultTheme.tokens[token]}".`);
      } else if (kind === "color" && !supports("color", value)) {
        problems.push(`"${token}" is not a color: "${value}".`);
      } else if (kind === "font" && !supports("font-family", value)) {
        problems.push(`"${token}" is not a font: "${value}".`);
      } else {
        given[token] = value;
      }
    }
  }
  if (problems.length > 0 || !baseTheme) return { problems };
  return {
    theme: {
      name: typeof name === "string" && name.trim() ? name.trim() : file.replace(/\.json$/i, ""),
      tokens: { ...baseTheme.tokens, ...given },
    },
  };
}

/** How much room each density gives, against Normal. */
export const DENSITY_SCALES: Record<string, number> = { compact: 0.8, normal: 1, roomy: 1.25 };

/** Which scales each size follows. Pictures and the visualizer keep their sizes. */
function scaleFor(token: TokenName, text: number, room: number): number {
  if (token.startsWith("font-size-")) return text;
  if (token === "size-row") return text * room;
  if (token.startsWith("space-")) return room;
  if (["size-sidebar", "size-queue", "size-album-caption", "size-waveform"].includes(token)) {
    return text;
  }
  return 1;
}

/** A theme's tokens with the text size and density applied, both as factors of 1. */
export function scaleTokens(tokens: Tokens, text: number, room: number): Tokens {
  const scaled = { ...tokens };
  for (const token of TOKEN_NAMES) {
    const factor = scaleFor(token, text, room);
    if (factor !== 1 && PIXELS.test(tokens[token])) {
      scaled[token] = `${Math.round(Number.parseFloat(tokens[token]) * factor)}px`;
    }
  }
  return scaled;
}

/** Whether a value is a full set of tokens, such as one kept from the last run. */
export function isTokens(value: unknown): value is Tokens {
  if (typeof value !== "object" || value === null) return false;
  const record = value as Record<string, unknown>;
  return TOKEN_NAMES.every((token) => typeof record[token] === "string");
}

/** Exposes tokens as CSS variables on the document root. */
export function applyTokens(tokens: Tokens): void {
  const root = document.documentElement.style;
  for (const [token, value] of Object.entries(tokens)) {
    root.setProperty(`--${token}`, value);
  }
}
