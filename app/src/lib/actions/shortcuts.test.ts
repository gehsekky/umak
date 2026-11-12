import { describe, expect, it } from "vitest";
import type { ShortcutAction } from "./registry";
import {
  actionForKey,
  actionGroup,
  assignShortcut,
  currentShortcuts,
  keyLabel,
  readChanged,
  unusableKey,
} from "./shortcuts";

const actions: ShortcutAction[] = [
  { id: "player.toggle", label: "Play or pause", shortcut: " " },
  { id: "player.stop", label: "Stop", shortcut: null },
  { id: "player.toggleMute", label: "Mute", shortcut: "m" },
  { id: "queue.togglePanel", label: "Queue", shortcut: "ctrl+q" },
];

describe("shortcuts", () => {
  it("are the defaults with the user's changes", () => {
    const current = currentShortcuts(actions, { "player.stop": "s", "player.toggleMute": null });
    expect(Object.fromEntries(current)).toEqual({
      "player.toggle": " ",
      "player.stop": "s",
      "player.toggleMute": null,
      "queue.togglePanel": "ctrl+q",
    });
  });

  it("give a key the user chose to that action, over one having it by default", () => {
    const changed = { "player.stop": "m" };
    expect(currentShortcuts(actions, changed).get("player.toggleMute")).toBe(null);
    expect(actionForKey("m", actions, changed)).toBe("player.stop");
    expect(actionForKey("x", actions, changed)).toBe(null);
  });

  it("read only keys and none from the setting", () => {
    expect(readChanged({ a: "s", b: null, c: 5, d: "" })).toEqual({ a: "s", b: null });
    expect(readChanged(["s"])).toEqual({});
    expect(readChanged(null)).toEqual({});
  });
});

describe("assigning a shortcut", () => {
  it("takes the key from the action that has it", () => {
    const { changed, takenFrom } = assignShortcut(actions, {}, "player.stop", "m");
    expect(changed).toEqual({ "player.stop": "m", "player.toggleMute": null });
    expect(takenFrom).toEqual(["player.toggleMute"]);
  });

  it("drops a change that has no default to go back to when its key is taken", () => {
    const before = { "player.stop": "s" };
    const { changed } = assignShortcut(actions, before, "player.toggleMute", "s");
    expect(changed).toEqual({ "player.toggleMute": "s" });
  });

  it("keeps an action's own default as no change, which resets it", () => {
    const before = { "player.toggleMute": "x", "player.stop": "m" };
    const { changed, takenFrom } = assignShortcut(actions, before, "player.toggleMute", "m");
    expect(changed).toEqual({});
    expect(takenFrom).toEqual(["player.stop"]);
    expect(assignShortcut(actions, { "player.stop": "s" }, "player.stop", null).changed).toEqual({});
  });

  it("clears a key without touching the others", () => {
    const { changed, takenFrom } = assignShortcut(actions, {}, "player.toggle", null);
    expect(changed).toEqual({ "player.toggle": null });
    expect(takenFrom).toEqual([]);
  });
});

describe("keys", () => {
  it("are named as people write them", () => {
    expect(keyLabel(" ")).toBe("Space");
    expect(keyLabel("ctrl+arrowleft")).toBe("Ctrl+Left");
    expect(keyLabel("ctrl+alt+shift+n")).toBe("Ctrl+Alt+Shift+N");
    expect(keyLabel("ctrl++")).toBe("Ctrl++");
    expect(keyLabel("f5")).toBe("F5");
    expect(keyLabel("shift+pagedown")).toBe("Shift+Page Down");
  });

  it("cannot be a modifier alone, Tab or a key that does not say which it is", () => {
    expect(unusableKey("ctrl+control")).toBe("");
    expect(unusableKey("shift")).toBe("");
    expect(unusableKey("tab")).toMatch(/Tab/);
    expect(unusableKey("dead")).toMatch(/does not say/);
    expect(unusableKey("ctrl+s")).toBe(null);
    expect(unusableKey("ctrl++")).toBe(null);
  });

  it("are listed under the part of the action they belong to", () => {
    expect(actionGroup("nav.library")).toBe("Go to");
    expect(actionGroup("player.stop")).toBe("Playback");
  });
});
