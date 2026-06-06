import { defineConfig } from "@playwright/test";

// Browser flows against a running Kapto. `cargo xtask ui e2e` starts one on
// free ports, says where in KAPTO_URL, and runs each spec in Docker.
export default defineConfig({
  testDir: "e2e",
  // A flow that fails says so the first time; no retries.
  retries: 0,
  workers: 1,
  timeout: 60_000,
  reporter: "list",
  use: {
    baseURL: process.env.KAPTO_URL ?? "http://localhost:7700",
    browserName: "chromium",
  },
});
