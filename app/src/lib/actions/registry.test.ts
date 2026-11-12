import { describe, expect, it, vi } from "vitest";
import {
  actionLabel,
  keyBelongsToElement,
  onActionError,
  registerAction,
  runAction,
  runShortcut,
  shortcutActions,
  shortcutName,
} from "./registry";

const key = (key: string, extra = {}) => ({
  key,
  ctrlKey: false,
  metaKey: false,
  shiftKey: false,
  altKey: false,
  ...extra,
});

describe("action registry", () => {
  it("runs a registered action with its arguments and knows its label", async () => {
    const run = vi.fn();
    registerAction({ id: "playlists.create", label: "Create playlist", run });
    expect(actionLabel("playlists.create")).toBe("Create playlist");
    expect(await runAction("playlists.create", { name: "Mix" })).toBe(true);
    expect(run).toHaveBeenCalledWith({ name: "Mix" });
  });

  it("reports a failing action instead of throwing", async () => {
    const errors: string[] = [];
    onActionError((message) => errors.push(message));
    registerAction({
      id: "playlists.delete",
      label: "Delete",
      run: () => Promise.reject({ code: "notFound", message: "gone" }),
    });
    expect(await runAction("playlists.delete", { id: "x" })).toBe(false);
    expect(errors).toEqual(["gone"]);
  });

  it("runs the action bound to a shortcut and ignores other keys", async () => {
    const run = vi.fn();
    registerAction({ id: "playlists.startCreate", label: "New", shortcut: "ctrl+n", run });
    expect(runShortcut("ctrl+n")).toBe(true);
    await Promise.resolve();
    expect(run).toHaveBeenCalledOnce();
    expect(runShortcut("ctrl+q")).toBe(false);
  });

  it("lists the actions that can take a shortcut and runs them by the keys the user gave", async () => {
    const stop = vi.fn();
    registerAction({ id: "player.stop", label: "Stop", shortcut: null, run: stop });
    registerAction({ id: "tags.preview", label: "Preview", shortcut: false, run: vi.fn() });
    const listed = shortcutActions();
    expect(listed).toContainEqual({ id: "player.stop", label: "Stop", shortcut: null });
    expect(listed.map((action) => action.id)).not.toContain("tags.preview");
    expect(listed.map((action) => action.id)).not.toContain("playlists.create");
    expect(runShortcut("s")).toBe(false);
    expect(runShortcut("s", { "player.stop": "s" })).toBe(true);
    await Promise.resolve();
    expect(stop).toHaveBeenCalledOnce();
    expect(runShortcut("ctrl+n", { "playlists.startCreate": null })).toBe(false);
  });

  it("names key presses consistently", () => {
    expect(shortcutName(key("N", { ctrlKey: true }))).toBe("ctrl+n");
    expect(shortcutName(key("n", { metaKey: true }))).toBe("ctrl+n");
    expect(shortcutName(key("/"))).toBe("/");
    expect(shortcutName(key("F", { ctrlKey: true, shiftKey: true, altKey: true }))).toBe(
      "ctrl+alt+shift+f",
    );
  });
});

describe("keyBelongsToElement", () => {
  it("gives plain keys to what is being typed in or pressed", () => {
    expect(keyBelongsToElement("m", "INPUT")).toBe(true);
    expect(keyBelongsToElement(" ", "BUTTON")).toBe(true);
    expect(keyBelongsToElement("m", "DIV")).toBe(false);
    expect(keyBelongsToElement("m", null)).toBe(false);
  });

  it("gives arrows with a modifier to text boxes and sliders", () => {
    expect(keyBelongsToElement("ctrl+arrowleft", "INPUT")).toBe(true);
    expect(keyBelongsToElement("shift+arrowright", "INPUT")).toBe(true);
    expect(keyBelongsToElement("ctrl+arrowleft", "DIV")).toBe(false);
    expect(keyBelongsToElement("ctrl+arrowleft", "BUTTON")).toBe(false);
  });

  it("leaves other shortcuts working anywhere", () => {
    expect(keyBelongsToElement("ctrl+f", "INPUT")).toBe(false);
    expect(keyBelongsToElement("ctrl+q", "SELECT")).toBe(false);
  });
});
