// Conversion settings as the dialog edits them, and text describing batches. The core
// checks settings again; these rules only keep the form to choices it would accept.

import type { ConvertCodec } from "../api/generated/ConvertCodec";
import type { ConvertPlan } from "../api/generated/ConvertPlan";
import type { ConvertPreset } from "../api/generated/ConvertPreset";
import type { ConvertReport } from "../api/generated/ConvertReport";
import type { ConvertSettings } from "../api/generated/ConvertSettings";
import { countOf } from "../lists/format";

export const CODECS: readonly { codec: ConvertCodec; label: string }[] = [
  { codec: "flac", label: "FLAC" },
  { codec: "alac", label: "ALAC" },
  { codec: "mp3", label: "MP3" },
  { codec: "aac", label: "AAC" },
  { codec: "opus", label: "Opus" },
];

/** LAME's constant bitrates, in kbps. */
export const MP3_BITRATES = [32, 40, 48, 56, 64, 80, 96, 112, 128, 160, 192, 224, 256, 320];
export const BITRATE_RANGES: Partial<Record<ConvertCodec, { min: number; max: number }>> = {
  aac: { min: 32, max: 512 },
  opus: { min: 6, max: 510 },
};
const DEFAULT_BITRATES: Partial<Record<ConvertCodec, number>> = { aac: 256, opus: 160 };
const SAMPLE_RATES = [32000, 44100, 48000, 88200, 96000, 176400, 192000];
/** The highest rate each lossy format holds. Opus always plays at 48 kHz. */
const RATE_LIMITS: Partial<Record<ConvertCodec, number>> = { mp3: 48000, aac: 96000, opus: 0 };
export const BIT_DEPTHS = [16, 24];

export function isLossless(codec: ConvertCodec): boolean {
  return codec === "flac" || codec === "alac";
}

export function codecLabel(codec: ConvertCodec): string {
  return CODECS.find((entry) => entry.codec === codec)?.label ?? codec;
}

/** The rates a format's files can be limited to. */
export function sampleRatesFor(codec: ConvertCodec): number[] {
  const limit = RATE_LIMITS[codec];
  return limit === undefined ? SAMPLE_RATES : SAMPLE_RATES.filter((rate) => rate <= limit);
}

/** Settings for another format, keeping what still applies to it. */
export function withCodec(settings: ConvertSettings, codec: ConvertCodec): ConvertSettings {
  const rate = settings.maxSampleRate;
  const next: ConvertSettings = {
    codec,
    mp3Quality: null,
    bitrateKbps: null,
    maxSampleRate: rate !== null && sampleRatesFor(codec).includes(rate) ? rate : null,
    maxBitDepth: isLossless(codec) ? settings.maxBitDepth : null,
  };
  if (codec === "mp3") {
    if (settings.codec === "mp3") {
      next.mp3Quality = settings.mp3Quality;
      next.bitrateKbps = settings.bitrateKbps;
    } else {
      next.mp3Quality = 0;
    }
  } else if (!isLossless(codec)) {
    const range = BITRATE_RANGES[codec];
    const kept = settings.bitrateKbps;
    next.bitrateKbps =
      kept !== null && range && kept >= range.min && kept <= range.max
        ? kept
        : (DEFAULT_BITRATES[codec] ?? null);
  }
  return next;
}

/** A preset's format and quality, with the limits already chosen kept where they apply. */
export function applyPreset(settings: ConvertSettings, preset: ConvertPreset): ConvertSettings {
  const limited = withCodec(settings, preset.settings.codec);
  return {
    ...preset.settings,
    maxSampleRate: preset.settings.maxSampleRate ?? limited.maxSampleRate,
    maxBitDepth: preset.settings.maxBitDepth ?? limited.maxBitDepth,
  };
}

/** The preset with this format and quality, whatever the limits. */
export function matchingPreset(
  presets: readonly ConvertPreset[],
  settings: ConvertSettings,
): ConvertPreset | undefined {
  return presets.find(
    ({ settings: preset }) =>
      preset.codec === settings.codec &&
      preset.mp3Quality === settings.mp3Quality &&
      preset.bitrateKbps === settings.bitrateKbps,
  );
}

/** For example "44.1 kHz". */
export function formatRate(hz: number): string {
  return `${hz / 1000} kHz`;
}

/** For example "MP3 V0", "AAC 256 kbps" or "FLAC, at most 48 kHz and 16 bits". */
export function describeSettings(settings: ConvertSettings): string {
  let text = codecLabel(settings.codec);
  if (settings.mp3Quality !== null) text += ` V${settings.mp3Quality}`;
  else if (settings.bitrateKbps !== null) text += ` ${settings.bitrateKbps} kbps`;
  const limits: string[] = [];
  if (settings.maxSampleRate !== null) limits.push(formatRate(settings.maxSampleRate));
  if (settings.maxBitDepth !== null) limits.push(`${settings.maxBitDepth} bits`);
  return limits.length > 0 ? `${text}, at most ${limits.join(" and ")}` : text;
}

/** The files a retry would convert again. */
export function retryable(report: ConvertReport): number {
  return report.failed + report.canceled + report.pending;
}

/** For example "12 converted, 1 failed, 3 already in the folder". */
export function describeReport(report: ConvertReport): string {
  const parts: string[] = [];
  if (report.converted > 0 || retryable(report) === 0) {
    parts.push(`${report.converted.toLocaleString()} converted`);
  }
  if (report.failed > 0) parts.push(`${report.failed.toLocaleString()} failed`);
  if (report.canceled > 0) parts.push(`${report.canceled.toLocaleString()} canceled`);
  if (report.pending > 0) parts.push(`${report.pending.toLocaleString()} waiting`);
  if (report.exists > 0) parts.push(`${report.exists.toLocaleString()} already in the folder`);
  if (report.sameOutput > 0) {
    parts.push(`${report.sameOutput.toLocaleString()} held back for sharing an output name`);
  }
  if (report.missing > 0) parts.push(`${report.missing.toLocaleString()} missing`);
  if (report.incompleteTags > 0) {
    parts.push(`${countOf(report.incompleteTags, "file")} without every tag`);
  }
  if (report.addedToLibrary !== null) {
    parts.push(`${countOf(report.addedToLibrary, "track")} added to the library`);
  }
  if (report.libraryError !== null) parts.push(`not added to the library: ${report.libraryError}`);
  return parts.join(", ");
}

/** Whether a finished batch has something the user should look at. */
export function needsAttention(report: ConvertReport): boolean {
  return report.failed > 0 || report.libraryError !== null;
}

/** What a planned batch will do, one sentence per kind of file. */
export function describePlan(plan: ConvertPlan, codec: ConvertCodec): string[] {
  const lines = [`${countOf(plan.ready, "file")} will be converted to .${plan.extension}.`];
  if (plan.lossySources > 0) {
    const effect = isLossless(codec)
      ? "converting them to a lossless format only makes them larger"
      : "converting them again loses more of the audio";
    const lossy =
      plan.lossySources === plan.ready
        ? "All of them are"
        : `${plan.lossySources.toLocaleString()} of them are`;
    lines.push(`${lossy} lossy already, so ${effect}.`);
  }
  if (plan.exists > 0) {
    lines.push(`${countOf(plan.exists, "file")} already in the folder will be left alone.`);
  }
  if (plan.sameOutput > 0) {
    const shared = countOf(plan.sameOutput, "file");
    lines.push(`${shared} would share an output name, so none of them will be converted.`);
  }
  if (plan.missing > 0) lines.push(`${countOf(plan.missing, "file")} cannot be found on disk.`);
  return lines;
}

/** The dialog's last choices, offered again next time. */
export type ConvertChoices = {
  settings: ConvertSettings;
  folder: string;
  threads: number | null;
  addToLibrary: boolean;
};

const CHOICES_KEY = "umak.convert";

export function loadChoices(): ConvertChoices | null {
  try {
    const saved = localStorage.getItem(CHOICES_KEY);
    if (!saved) return null;
    const choices = JSON.parse(saved) as ConvertChoices;
    if (!CODECS.some((entry) => entry.codec === choices.settings?.codec)) return null;
    return choices;
  } catch {
    // Storage can be unavailable or hold something else; the dialog falls back to defaults.
    return null;
  }
}

export function saveChoices(choices: ConvertChoices): void {
  try {
    localStorage.setItem(CHOICES_KEY, JSON.stringify(choices));
  } catch {
    // Remembering choices is only a convenience.
  }
}
