import { defineConfig } from "vitest/config";

// Тесты стора (дедуп сообщений, слияние событий). Окружение — node:
// проверяем чистую логику, без DOM.
export default defineConfig({
  test: {
    environment: "node",
    include: ["src/**/*.test.ts"],
  },
});
