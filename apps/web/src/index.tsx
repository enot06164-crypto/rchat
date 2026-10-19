/* @refresh reload */
import { render } from "solid-js/web";
import App from "./App";
import { state } from "./services/store";
import "./styles/global.scss";

// Отладочный доступ для смоков и DevTools (только dev-сервер Vite).
if (import.meta.env.DEV) {
  (window as unknown as Record<string, unknown>).__rchat = { state };
}

const root = document.getElementById("root");
if (root === null) {
  throw new Error("#root не найден в index.html");
}

render(() => <App />, root);
