import { describe, expect, it } from "vitest";
import {
  BUILT_IN_THEMES,
  TEST_THEME,
  TOKEN_NAMES,
  checkTheme,
  isTokens,
  scaleTokens,
  type Supports,
} from "./theme";

/** A stand-in for `CSS.supports`, which tests do not have. */
const supports: Supports = (property, value) =>
  property === "color" ? /^(#[0-9a-f]{3,8}|rgba?\(.*\)|[a-z]+)$/i.test(value) : value.length > 0;

function problems(text: string): string[] {
  const checked = checkTheme(text, "mine.json", supports);
  return "problems" in checked ? checked.problems : [];
}

describe("themes", () => {
  it("come with the app complete, and pass the checks a file gets", () => {
    for (const theme of [...Object.values(BUILT_IN_THEMES), TEST_THEME]) {
      expect(isTokens(theme.tokens)).toBe(true);
      expect(Object.keys(theme.tokens).sort()).toEqual([...TOKEN_NAMES].sort());
      expect(problems(JSON.stringify({ tokens: theme.tokens }))).toEqual([]);
    }
  });

  it("from a file take the tokens they leave out from their base", () => {
    const checked = checkTheme(
      JSON.stringify({ name: "Mine", base: "light", tokens: { "color-accent": "#ff0000" } }),
      "mine.json",
      supports,
    );
    expect(checked).toEqual({
      theme: {
        name: "Mine",
        tokens: { ...BUILT_IN_THEMES.light.tokens, "color-accent": "#ff0000" },
      },
    });
    const unnamed = checkTheme('{"tokens": {}}', "Night Owl.json", supports);
    expect(unnamed).toEqual({ theme: { name: "Night Owl", tokens: BUILT_IN_THEMES.default.tokens } });
  });

  it("from a file say what is wrong with them", () => {
    expect(problems("{oops")[0]).toMatch(/^It is not valid JSON: /);
    expect(problems("[]")).toEqual(['It needs to be a JSON object with a "tokens" object in it.']);
    expect(problems('{"name": "x"}')).toEqual(['It needs a "tokens" object.']);
    expect(
      problems(
        JSON.stringify({
          base: "sepia",
          colors: {},
          tokens: {
            "color-accnt": "#fff",
            "color-text": 7,
            "size-row": "2em",
            "color-bg": "not a color!",
          },
        }),
      ),
    ).toEqual([
      '"colors" is not part of a theme; it takes "name", "base" and "tokens".',
      '"base" needs to be one of default, light.',
      'There is no token named "color-accnt".',
      '"color-text" needs to be text, such as "#e8eaed".',
      '"size-row" needs a size in pixels, such as "30px".',
      '"color-bg" is not a color: "not a color!".',
    ]);
  });

  it("scale text and room by their own factors, rounding to whole pixels", () => {
    const tokens = BUILT_IN_THEMES.default.tokens;
    const scaled = scaleTokens(tokens, 1.25, 0.8);
    expect(scaled["font-size-body"]).toBe("18px");
    expect(scaled["space-md"]).toBe("13px");
    expect(scaled["size-row"]).toBe("30px");
    expect(scaled["size-sidebar"]).toBe("288px");
    expect(scaled["size-album-cover"]).toBe(tokens["size-album-cover"]);
    expect(scaled["color-accent"]).toBe(tokens["color-accent"]);
    expect(scaleTokens(tokens, 1, 1)).toEqual(tokens);
  });
});
