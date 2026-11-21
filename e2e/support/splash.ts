import type { Page } from "@playwright/test";

/**
 * Wait for the shell's first-paint layer to be gone before photographing the app.
 *
 * `index.html` paints a splash over the empty shell and hands the screen to the app once the
 * session read answers (T30); the hand-off cross-fades for 200ms. A capture taken inside that
 * window would photograph the veil rather than the screen it is meant to document, so every
 * screenshot in this suite waits the layer out first. Resolves immediately on a page that has
 * already dropped it, which is the case for everything after the first document load.
 */
export async function awaitSplashGone(page: Page): Promise<void> {
  await page.locator("#splash").waitFor({ state: "detached", timeout: 10_000 });
}
