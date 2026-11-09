import { describe, expect, it } from "vitest";
import type { ConvertPlan } from "../api/generated/ConvertPlan";
import type { ConvertPreset } from "../api/generated/ConvertPreset";
import type { ConvertReport } from "../api/generated/ConvertReport";
import type { ConvertSettings } from "../api/generated/ConvertSettings";
import {
  applyPreset,
  describePlan,
  describeReport,
  describeSettings,
  matchingPreset,
  retryable,
  sampleRatesFor,
  withCodec,
} from "./options";

function settings(changes: Partial<ConvertSettings>): ConvertSettings {
  return {
    codec: "flac",
    mp3Quality: null,
    bitrateKbps: null,
    maxSampleRate: null,
    maxBitDepth: null,
    ...changes,
  };
}

function report(changes: Partial<ConvertReport>): ConvertReport {
  return {
    pending: 0,
    converted: 0,
    failed: 0,
    canceled: 0,
    exists: 0,
    sameOutput: 0,
    missing: 0,
    incompleteTags: 0,
    addedToLibrary: null,
    libraryError: null,
    ...changes,
  };
}

const PRESETS: ConvertPreset[] = [
  { id: "flac", name: "FLAC", settings: settings({}) },
  { id: "mp3-v0", name: "MP3 V0", settings: settings({ codec: "mp3", mp3Quality: 0 }) },
  { id: "aac-256", name: "AAC 256 kbps", settings: settings({ codec: "aac", bitrateKbps: 256 }) },
];

describe("withCodec", () => {
  it("keeps the limits that still apply", () => {
    const flac = settings({ maxSampleRate: 96000, maxBitDepth: 16 });
    expect(withCodec(flac, "alac")).toEqual(
      settings({ codec: "alac", maxSampleRate: 96000, maxBitDepth: 16 }),
    );
    // MP3 holds at most 48 kHz and sets its own depth.
    expect(withCodec(flac, "mp3")).toEqual(settings({ codec: "mp3", mp3Quality: 0 }));
    expect(withCodec(flac, "aac")).toEqual(
      settings({ codec: "aac", bitrateKbps: 256, maxSampleRate: 96000 }),
    );
  });

  it("gives a lossy format a quality it accepts", () => {
    const aac = settings({ codec: "aac", bitrateKbps: 320 });
    expect(withCodec(aac, "opus").bitrateKbps).toBe(320);
    expect(withCodec(settings({ codec: "aac", bitrateKbps: 512 }), "opus").bitrateKbps).toBe(160);
    expect(withCodec(aac, "flac")).toEqual(settings({}));
    expect(withCodec(settings({ maxSampleRate: 44100 }), "opus").maxSampleRate).toBeNull();
  });
});

describe("presets", () => {
  it("finds the preset whatever the limits", () => {
    const limited = settings({ codec: "mp3", mp3Quality: 0, maxSampleRate: 44100 });
    expect(matchingPreset(PRESETS, limited)?.id).toBe("mp3-v0");
    expect(matchingPreset(PRESETS, settings({ codec: "mp3", mp3Quality: 2 }))).toBeUndefined();
  });

  it("keeps the chosen limits when one is applied", () => {
    const limited = settings({ maxSampleRate: 48000, maxBitDepth: 16 });
    expect(applyPreset(limited, PRESETS[2])).toEqual(
      settings({ codec: "aac", bitrateKbps: 256, maxSampleRate: 48000 }),
    );
  });

  it("lists the rates each format holds", () => {
    expect(sampleRatesFor("mp3")).toEqual([32000, 44100, 48000]);
    expect(sampleRatesFor("opus")).toEqual([]);
    expect(sampleRatesFor("flac")).toContain(192000);
  });
});

describe("text", () => {
  it("describes settings", () => {
    expect(describeSettings(settings({ codec: "mp3", mp3Quality: 0 }))).toBe("MP3 V0");
    expect(describeSettings(settings({ codec: "aac", bitrateKbps: 256 }))).toBe("AAC 256 kbps");
    expect(describeSettings(settings({ maxSampleRate: 44100, maxBitDepth: 16 }))).toBe(
      "FLAC, at most 44.1 kHz and 16 bits",
    );
  });

  it("describes a batch's outcome", () => {
    expect(describeReport(report({}))).toBe("0 converted");
    expect(
      describeReport(
        report({ converted: 1200, failed: 2, exists: 3, incompleteTags: 1, addedToLibrary: 1200 }),
      ),
    ).toBe(
      "1,200 converted, 2 failed, 3 already in the folder, 1 file without every tag, " +
        "1,200 tracks added to the library",
    );
    expect(describeReport(report({ canceled: 4 }))).toBe("4 canceled");
    expect(retryable(report({ failed: 1, canceled: 2, pending: 3, converted: 9 }))).toBe(6);
  });

  it("describes a plan", () => {
    const plan: ConvertPlan = {
      ready: 10,
      exists: 1,
      sameOutput: 2,
      missing: 0,
      lossySources: 10,
      extension: "flac",
      exampleOutput: null,
      problems: [],
    };
    expect(describePlan(plan, "flac")).toEqual([
      "10 files will be converted to .flac.",
      "All of them are lossy already, so converting them to a lossless format only makes " +
        "them larger.",
      "1 file already in the folder will be left alone.",
      "2 files would share an output name, so none of them will be converted.",
    ]);
    expect(describePlan({ ...plan, lossySources: 3, exists: 0, sameOutput: 0 }, "mp3")).toEqual([
      "10 files will be converted to .flac.",
      "3 of them are lossy already, so converting them again loses more of the audio.",
    ]);
  });
});
