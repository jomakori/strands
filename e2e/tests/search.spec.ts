import { expect, test } from "@playwright/test";

// Ticket T11. The sheet is mounted once by the app shell, so these assertions start from
// `/` in the release bundle CI serves. No API is reachable behind that static server,
// which is deliberate: the sheet must still open, focus its field and keep the page within
// the viewport when the search endpoint is unavailable — the offline/empty state is the
// baseline, and a live server only ever adds rows to it.
test.describe("search sheet", () => {
  test("opens on '/' and closes on Esc", async ({ page }, testInfo) => {
    await page.goto("/");
    // The grid proves the shell (and therefore the sheet's host layout) has rendered.
    await expect(page.locator("#grid [data-tag-id]").first()).toBeVisible();

    const sheet = page.locator("#search-sheet");
    await expect(sheet).toHaveCount(0);

    await page.keyboard.press("/");
    await expect(sheet).toBeVisible();
    await expect(page.locator("#search-input")).toBeFocused();
    // Nothing typed yet: the sheet is showing recents, not a failed search.
    await expect(sheet.locator(".sheet__heading")).toHaveText("Recent");

    const open = await page.evaluate(() => ({
      clientWidth: document.documentElement.clientWidth,
      scrollWidth: document.documentElement.scrollWidth,
    }));
    expect(
      open.scrollWidth,
      `the open sheet scrolls horizontally: ${open.scrollWidth}px in ${open.clientWidth}px`,
    ).toBeLessThanOrEqual(open.clientWidth);

    await page.screenshot({
      path: `captures/${testInfo.project.name}-search-open.png`,
    });

    await page.keyboard.press("Escape");
    await expect(sheet).toHaveCount(0);

    // Cmd/Ctrl+K is the second door into the same sheet.
    await page.keyboard.press("Control+k");
    await expect(sheet).toBeVisible();
    await expect(page.locator("#search-input")).toBeFocused();
    await page.keyboard.press("Escape");
    await expect(sheet).toHaveCount(0);
  });

  test("a submitted query switches the sheet from recents to results", async ({
    page,
  }, testInfo) => {
    await page.goto("/");
    // Wait for the shell before typing: the keydown listener belongs to the mounted app.
    await expect(page.locator("#grid [data-tag-id]").first()).toBeVisible();
    await page.keyboard.press("/");

    const input = page.locator("#search-input");
    await input.fill("kumquat");
    await input.press("Enter");

    const sheet = page.locator("#search-sheet");
    await expect(sheet.locator(".sheet__heading")).toHaveText("Results");
    // With no API behind the static bundle the answer is empty; what matters is that the
    // request path completed and the sheet stayed open instead of blowing up.
    await expect(sheet.locator(".sheet__empty")).toBeVisible();

    await page.screenshot({
      path: `captures/${testInfo.project.name}-search-results.png`,
    });
  });

  test("reaches the sheet from a tag's own route", async ({ page }) => {
    await page.goto("/");
    const firstTag = page.locator("#grid [data-tag-id]").first();
    const tagId = await firstTag.getAttribute("data-tag-id");
    await firstTag.click();
    // Client-side navigation: poll the URL rather than waiting for a `load` event.
    await expect(page).toHaveURL(new RegExp(`/tags/${tagId}$`));

    // Same single mount: navigating away from `/` must not take the sheet with it.
    await page.keyboard.press("/");
    await expect(page.locator("#search-sheet")).toBeVisible();
    await page.keyboard.press("Escape");
    await expect(page.locator("#search-sheet")).toHaveCount(0);
  });
});
