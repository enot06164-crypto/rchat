/* @refresh reload */
import { render } from "solid-js/web";
import App from "./App";
import { state } from "./services/store";
import "./styles/global.scss";

// Отладочный доступ для смоков и DevTools (только dev-сервер Vite).
if (import.meta.env.DEV) {
  (window as unknown as Record<string, unknown>).__rchat = { state };
  // Ловим ошибки страницы — их видно из смоков (иначе экран молча «висит»).
  (window as unknown as Record<string, unknown>).__errs = [];
  window.addEventListener("error", (event) => {
    const list = (window as unknown as Record<string, unknown>).__errs as string[];
    list.push(String(event.message) + " @ " + String(event.filename) + ":" + String(event.lineno) + " " + String((event as unknown as { error?: { stack?: string } }).error?.stack ?? ""));
  });
  window.addEventListener("unhandledrejection", (event) => {
    const list = (window as unknown as Record<string, unknown>).__errs as string[];
    const reason = event.reason as { message?: string; stack?: string } | undefined;
    list.push("promise: " + String(reason?.message ?? event.reason) + " " + String(reason?.stack ?? ""));
  });
}

const root = document.getElementById("root");
if (root === null) {
  throw new Error("#root не найден в index.html");
}

render(() => <App />, root);
