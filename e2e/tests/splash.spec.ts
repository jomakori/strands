import { expect, test } from "@playwright/test";

// The splash is markup in `index.html`, so it is on screen before the wasm bundle can even run
// and it belongs to a document load rather than to a route. Both halves of that are only
// observable at specific moments: the first needs the session probe still in flight, the second
// needs a navigation that is not a reload. This spec holds the one request the app makes before
// it draws anything, and takes its picture inside that window.

test.describe("splash", () => {
  test("covers first paint, then hands the screen to the grid", async ({
    page,
  }, testInfo) => {
    let release!: () => void;
    const held = new Promise<void>((resolve) => {
      release = resolve;
    });
    await page.route("**/api/v1/me", async (route) => {
      await held;
      await route.continue();
    });

    await page.goto("/", { waitUntil: "commit" });

    const splash = page.getByTestId("splash");
    await expect(splash).toBeVisible();
    await expect(splash.locator(".splash-word")).toHaveText("Strands");
    await expect(splash.locator(".splash-mark")).toBeVisible();
    // Full bleed and opaque: nothing of the unstyled document shows through it.
    const cover = await splash.evaluate((node) => {
      const box = node.getBoundingClientRect();
      const style = getComputedStyle(node);
      return {
        width: box.width,
        height: box.height,
        viewport: [window.innerWidth, window.innerHeight],
        opacity: style.opacity,
        background: style.backgroundColor,
      };
    });
    expect(cover.width).toBe(cover.viewport[0]);
    expect(cover.height).toBe(cover.viewport[1]);
    expect(cover.opacity).toBe("1");
    expect(cover.background).not.toBe("rgba(0, 0, 0, 0)");

    await page.screenshot({ path: `captures/${testInfo.project.name}-splash.png` });

    release();

    await expect(page.locator("#grid [data-tag-id]").first()).toBeVisible();
    // Removed, not merely faded: nothing is left over the grid to swallow a tap or a tab stop.
    await expect(splash).toHaveCount(0);
  });

  test("never comes back on in-app navigation", async ({ page }) => {
    await page.goto("/");
    await expect(page.locator("#grid [data-tag-id]").first()).toBeVisible();
    const splash = page.getByTestId("splash");
    await expect(splash).toHaveCount(0);

    // A route change replaces what the router owns; the shell around it — and with it the
    // splash — is never painted a second time.
    await page.locator("#grid [data-tag-id]").first().click();
    await expect(page).toHaveURL(/\/tags\//);
    await expect(splash).toHaveCount(0);

    await page.goBack();
    await expect(page.locator("#grid [data-tag-id]").first()).toBeVisible();
    await expect(splash).toHaveCount(0);
  });

  test("honours prefers-reduced-motion by hiding without a fade", async ({ page }) => {
    await page.emulateMedia({ reducedMotion: "reduce" });

    // Whether the layer faded is a fact about the DOM, not about how fast the machine booted the
    // wasm: watch the attribute the fade is driven by, from the document's first frame.
    await page.addInitScript(() => {
      const seen = { leaving: false };
      (window as unknown as { __splashWatch: typeof seen }).__splashWatch = seen;
      const track = () => {
        const splash = document.getElementById("splash");
        if (!splash) {
          requestAnimationFrame(track);
          return;
        }
        new MutationObserver(() => {
          if (splash.dataset.leaving === "true") seen.leaving = true;
        }).observe(splash, { attributes: true, attributeFilter: ["data-leaving"] });
      };
      requestAnimationFrame(track);
    });

    let release!: () => void;
    const held = new Promise<void>((resolve) => {
      release = resolve;
    });
    await page.route("**/api/v1/me", async (route) => {
      await held;
      await route.continue();
    });

    await page.goto("/", { waitUntil: "commit" });
    const splash = page.getByTestId("splash");
    await expect(splash).toBeVisible();
    const duration = await splash.evaluate(
      (node) => getComputedStyle(node).transitionDuration,
    );
    // `transition: none` computes to a hair over zero, not the 200ms the app fades with.
    expect(parseFloat(duration)).toBeLessThanOrEqual(0.001);

    release();
    // Removed outright, so the wait is for the app's own read, never a fade to sit through.
    await expect(splash).toHaveCount(0, { timeout: 5000 });
    const watch = await page.evaluate(
      () => (window as unknown as { __splashWatch: { leaving: boolean } }).__splashWatch,
    );
    expect(watch.leaving, "splash faded instead of hiding outright").toBe(false);
  });

  test("the shell ships a real icon, a real manifest and a real logo", async ({
    page,
  }) => {
    await page.goto("/");

    const iconHref = await page
      .locator('link[rel="icon"]')
      .getAttribute("href");
    const manifestHref = await page
      .locator('link[rel="manifest"]')
      .getAttribute("href");
    expect(iconHref, "index.html has no rel=icon").toBeTruthy();
    expect(manifestHref, "index.html has no rel=manifest").toBeTruthy();

    const icon = await page.request.get(iconHref!);
    expect(icon.status()).toBe(200);
    expect(icon.headers()["content-type"]).toContain("svg");

    const manifest = await page.request.get(manifestHref!);
    expect(manifest.status()).toBe(200);
    const parsed = await manifest.json();
    expect(parsed.name).toBe("Strands");
    expect(parsed.short_name).toBe("Strands");
    expect(parsed.theme_color).toBe("#090d16");
    expect(parsed.background_color).toBe("#090d16");
    expect(parsed.icons).toHaveLength(1);

    const manifestIcon = await page.request.get(parsed.icons[0].src);
    expect(manifestIcon.status()).toBe(200);

    // The mark the header draws is the same file as an asset, not a second copy of the paths.
    const mark = page.locator(".brand-mark");
    await expect(mark).toBeVisible();
    const src = await mark.getAttribute("src");
    expect(src).toMatch(/assets\/logo-.*\.svg$/);
    const logo = await page.request.get(src!);
    expect(logo.status()).toBe(200);
    expect(logo.headers()["content-type"]).toContain("svg");
  });
});
