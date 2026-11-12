import { describe, expect, it } from "vitest";
import type { SettingDefinition } from "../api/generated/SettingDefinition";
import type { SettingValue } from "../api/generated/SettingValue";
import { changedKeys, formatNumber, moveOption, orderedOptions, sections } from "./show";

function setting(changes: Partial<SettingDefinition>): SettingDefinition {
  return {
    key: "a",
    section: "Audio",
    label: "A",
    help: "",
    kind: { type: "toggle" },
    default: false,
    hidden: false,
    ...changes,
  };
}

const definitions = [
  setting({ key: "volume", label: "Volume", hidden: true }),
  setting({
    key: "gain",
    label: "ReplayGain",
    help: "Evens out loudness.",
    kind: {
      type: "choice",
      options: [
        { value: "off", label: "Off" },
        { value: "album", label: "Album" },
      ],
    },
  }),
  setting({ key: "theme", section: "Appearance", label: "Theme" }),
  setting({ key: "preamp", label: "Preamp", help: "Added on top of ReplayGain." }),
];

function value(key: string, isDefault: boolean): SettingValue {
  return { key, value: null, isDefault, problem: null };
}

describe("settings sections", () => {
  it("groups settings in the order given and leaves out hidden ones", () => {
    expect(sections(definitions, "")).toEqual([
      { name: "Audio", settings: [definitions[1], definitions[3]] },
      { name: "Appearance", settings: [definitions[2]] },
    ]);
  });

  it("keeps the settings holding every word searched for, in any case", () => {
    const keys = (query: string) =>
      sections(definitions, query).flatMap((section) => section.settings.map((found) => found.key));
    expect(keys("replaygain")).toEqual(["gain", "preamp"]);
    expect(keys("loudness EVENS")).toEqual(["gain"]);
    expect(keys("album")).toEqual(["gain"]);
    expect(keys("appearance")).toEqual(["theme"]);
    expect(keys("volume")).toEqual([]);
  });

  it("finds the settings changed from their defaults", () => {
    const values = { gain: value("gain", false), preamp: value("preamp", true) };
    expect(changedKeys(definitions, values)).toEqual(["gain"]);
  });
});

describe("setting numbers", () => {
  it("show the step's decimals, a sign below zero and the unit", () => {
    const decibels = { min: -12, step: 0.5, unit: "dB" };
    expect(formatNumber(2, decibels)).toBe("+2.0 dB");
    expect(formatNumber(-1.5, decibels)).toBe("-1.5 dB");
    expect(formatNumber(0, decibels)).toBe("0.0 dB");
    expect(formatNumber(5, { min: 1, step: 1, unit: null })).toBe("5");
    expect(formatNumber(110, { min: 80, step: 5, unit: "%" })).toBe("110%");
  });
});

describe("order settings", () => {
  const options = ["a", "b", "c", "d"].map((value) => ({ value, label: value.toUpperCase() }));

  it("list the chosen options in their order, then the rest", () => {
    expect(orderedOptions(options, ["c", "a"]).map((option) => option.value)).toEqual([
      "c",
      "a",
      "b",
      "d",
    ]);
  });

  it("move one option a place, staying within the list", () => {
    expect(moveOption(["a", "b", "c"], 1, -1)).toEqual(["b", "a", "c"]);
    expect(moveOption(["a", "b", "c"], 2, 1)).toEqual(["a", "b", "c"]);
  });
});
