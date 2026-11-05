import { describe, expect, it, vi } from "vitest";
import {
  actionLabel,
  onActionError,
  registerAction,
  runAction,
  runShortcut,
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

  it("names key presses consistently", () => {
    expect(shortcutName(key("N", { ctrlKey: true }))).toBe("ctrl+n");
    expect(shortcutName(key("n", { metaKey: true }))).toBe("ctrl+n");
    expect(shortcutName(key("/"))).toBe("/");
    expect(shortcutName(key("F", { ctrlKey: true, shiftKey: true, altKey: true }))).toBe(
      "ctrl+alt+shift+f",
    );
  });
});
