import { expect, test } from "@playwright/test";

import { awaitSplashGone } from "../support/splash";

// The top row is the mockup's: brand left, the search pill centred, tools right.
test.describe("top row", () => {
  test("the pill opens the search sheet", async ({ page }, testInfo) => {
    await page.goto("/");
    const pill = page.locator(".search-trigger");
    await expect(pill).toBeVisible();
    await expect(pill).toHaveText(/Search tags and threads/);

    await pill.click();
    await expect(page.locator("#search-sheet")).toBeVisible();
    await expect(page.locator("#search-input")).toBeFocused();

    await awaitSplashGone(page);
    await page.screenshot({
      path: `captures/${testInfo.project.name}-nav-search.png`,
      fullPage: true,
    });
  });

  test("the tools reach settings and the shortcut map", async ({
    page,
  }, testInfo) => {
    await page.goto("/");

    await page.locator(".icon-btn[aria-label='Settings']").click();
    await expect(page).toHaveURL(/\/settings$/);

    await page.goto("/");
    const help = page.locator(".help-btn");
    if (await help.isVisible()) {
      await help.click();
      await expect(page.locator("#shortcut-map")).toBeVisible();
      await page.keyboard.press("Escape");
      await expect(page.locator("#shortcut-map")).toHaveCount(0);
    }

    await awaitSplashGone(page);
    await page.screenshot({
      path: `captures/${testInfo.project.name}-nav-row.png`,
      fullPage: true,
    });
  });
});
