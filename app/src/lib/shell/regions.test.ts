import type { Component } from "svelte";
import { describe, expect, it } from "vitest";
import { registerView, viewsIn } from "./regions";

const component = (() => {}) as unknown as Component;

describe("view regions", () => {
  it("keeps registered views in order, per region", () => {
    registerView("status", { id: "a", component });
    registerView("status", { id: "b", component });
    registerView("sidebar", { id: "c", component });
    expect(viewsIn("status").map((view) => view.id)).toEqual(["a", "b"]);
    expect(viewsIn("sidebar").map((view) => view.id)).toEqual(["c"]);
  });

  it("replaces a view registered again under the same ID", () => {
    const other = (() => {}) as unknown as Component;
    registerView("main", { id: "x", component });
    registerView("main", { id: "x", component: other });
    expect(viewsIn("main")).toEqual([{ id: "x", component: other }]);
  });

  it("reports an empty region as empty", () => {
    expect(viewsIn("main").filter((view) => view.id === "none")).toEqual([]);
  });
});
