import { svelte } from "@sveltejs/vite-plugin-svelte";
import { defineConfig } from "vitest/config";

// `npm run dev` serves the UI with hot reload and passes API requests to a
// running `kapto serve`.
export default defineConfig({
  plugins: [svelte()],
  build: {
    outDir: "dist",
    assetsDir: "assets",
    // Kapto's page policy allows no inline scripts or styles.
    assetsInlineLimit: 0,
  },
  server: {
    proxy: { "/api": "http://127.0.0.1:7700" },
  },
  resolve: process.env.VITEST ? { conditions: ["browser"] } : undefined,
  test: {
    environment: "jsdom",
    // Lets Testing Library clean up after each test by itself.
    globals: true,
    include: ["src/**/*.test.ts"],
    setupFiles: ["src/test-setup.ts"],
  },
});
