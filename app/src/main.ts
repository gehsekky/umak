import { mount } from "svelte";
import App from "./App.svelte";
import "./app.css";
import { applyTheme, selectTheme } from "./lib/theme/theme";

applyTheme(selectTheme(import.meta.env.VITE_UMAK_THEME));

const target = document.getElementById("app");
if (!target) {
  throw new Error("Missing #app element");
}

export default mount(App, { target });
