import { expect, test } from "@playwright/test";

import { awaitSplashGone } from "../support/splash";

// Runs against the release Wasm bundle CI just built, so a green run means the
// shipped artifacts render — not that a local dev server did.
test.describe("home grid", () => {
  test("renders every seeded tag without horizontal overflow", async ({
    page,
  }, testInfo) => {
    await page.goto("/");

    const tags = page.locator("#grid [data-tag-id]");
    await expect(tags).toHaveCount(16);
    await expect(page.locator(".fchip")).toHaveCount(5);

    const { clientWidth, scrollWidth } = await page.evaluate(() => ({
      clientWidth: document.documentElement.clientWidth,
      scrollWidth: document.documentElement.scrollWidth,
    }));
    expect(
      scrollWidth,
      `page scrolls horizontally: content is ${scrollWidth}px wide in a ${clientWidth}px viewport`,
    ).toBeLessThanOrEqual(clientWidth);

    await awaitSplashGone(page);
    await page.screenshot({
      path: `captures/${testInfo.project.name}-home.png`,
      fullPage: true,
    });
  });

  test("a category chip narrows the visible set", async ({ page }, testInfo) => {
    await page.goto("/");

    const tags = page.locator("#grid [data-tag-id]");
    await expect(tags).toHaveCount(16);

    const travel = page.locator('[data-chip="travel"]');
    await travel.click();
    await expect(travel).toHaveAttribute("aria-pressed", "true");

    const visible = await tags.count();
    expect(visible).toBeGreaterThan(0);
    expect(visible).toBeLessThan(16);
    await expect(page.locator("#feed-count")).toContainText("tags");

    await awaitSplashGone(page);
    await page.screenshot({
      path: `captures/${testInfo.project.name}-travel-filter.png`,
      fullPage: true,
    });
  });
});
