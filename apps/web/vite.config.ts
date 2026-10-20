import { defineConfig } from "vite";
import solid from "vite-plugin-solid";

// In dev the browser talks to the Vite server (5173); `/ws` is proxied to the
// Python gateway from bindings/python (stage H, `python3 -m rchat.gateway`).
// The same proxy is configured for `vite preview` so a production bundle can
// be exercised against a locally running gateway without extra tooling.
const wsProxy = {
  "/ws": {
    target: "ws://127.0.0.1:8080",
    ws: true,
  },
};

// Vite (>= 5.4.12) проверяет Host заголовка: запрос с чужим именем блокируется
// («Blocked request»).  Чтобы клиент открывался через туннель (cloudpub и
// подобные), нужно разрешить имя хоста.  Имя поддомена у туннеля меняется при
// каждом запуске, поэтому разрешаем домен целиком («.cloudpub.ru» — любой его
// поддомен).  Другие туннели добавляются сюда же; для разовой проверки можно
// поставить `allowedHosts: true` (тогда разрешены все имена — защита от
// DNS-rebinding при этом отключается, так что только для локальных нужд).
const allowedHosts = [".cloudpub.ru"];

export default defineConfig({
  plugins: [solid()],
  server: {
    port: 5173,
    allowedHosts,
    proxy: wsProxy,
  },
  preview: {
    port: 4173,
    allowedHosts,
    proxy: wsProxy,
  },
  build: {
    target: "es2022",
    sourcemap: true,
  },
});
