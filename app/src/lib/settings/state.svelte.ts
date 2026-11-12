// Every setting and its value, kept current by the core's events.

import { call, onCoreEvent } from "../api/client";
import type { SettingDefinition } from "../api/generated/SettingDefinition";
import type { SettingValue } from "../api/generated/SettingValue";

class SettingsState {
  definitions = $state.raw<SettingDefinition[]>([]);
  values = $state.raw<Record<string, SettingValue>>({});

  private take(changed: SettingValue[]): void {
    const values = { ...this.values };
    for (const setting of changed) values[setting.key] = setting;
    this.values = values;
  }

  value(key: string): unknown {
    return this.values[key]?.value;
  }

  async set(key: string, value: unknown): Promise<void> {
    this.take([await call("settings.set", { key, value })]);
  }

  async reset(keys: string[]): Promise<void> {
    this.take((await call("settings.reset", { keys })).settings);
  }

  /** Loads the settings. Returns how many saved values made no sense and were not used. */
  async start(): Promise<{ stop: () => void; problems: number }> {
    const stop = await onCoreEvent((event) => {
      if (event.type === "settingsChanged") this.take(event.settings);
    });
    const list = await call("settings.list", {});
    this.definitions = list.definitions;
    this.take(list.values);
    return { stop, problems: list.values.filter((setting) => setting.problem).length };
  }
}

export const settings = new SettingsState();
