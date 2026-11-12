import { mount } from "svelte";
import App from "./App.svelte";
import "./app.css";
import { startingTokens } from "./lib/theme/appearance.svelte";
import { applyTokens } from "./lib/theme/theme";

applyTokens(startingTokens());

const target = document.getElementById("app");
if (!target) {
  throw new Error("Missing #app element");
}

export default mount(App, { target });
