import { defineConfig, devices } from "@playwright/test";

// Both viewports run every spec, so each capture lands under its own project name.
// CI supplies the served bundle address; the default matches the port it serves on.
export default defineConfig({
  testDir: "./tests",
  outputDir: "./test-results",
  fullyParallel: true,
  forbidOnly: !!process.env.CI,
  retries: 0,
  reporter: [
    ["list"],
    ["json", { outputFile: "./test-results/results.json" }],
  ],
  use: {
    baseURL: process.env.STRANDS_BASE_URL ?? "http://127.0.0.1:4173",
    trace: "off",
    video: "off",
  },
  projects: [
    {
      name: "mobile-360",
      use: {
        ...devices["Desktop Chrome"],
        viewport: { width: 360, height: 800 },
        deviceScaleFactor: 2,
        isMobile: true,
        hasTouch: true,
      },
    },
    {
      name: "desktop-1440",
      use: {
        ...devices["Desktop Chrome"],
        viewport: { width: 1440, height: 1000 },
      },
    },
  ],
});
