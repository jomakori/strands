import { expect, test } from "@playwright/test";

import { awaitSplashGone } from "../support/splash";

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

    await awaitSplashGone(page);
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

    // T18: a search that cannot be answered is a failure with a control, never a
    // dead sentence. The static bundle has no API, so the first request fails; the
    // button must re-issue it and the second answer must paint.
    const notice = sheet.locator(".load-error");
    await expect(notice).toBeVisible();
    await expect(notice.locator(".load-error-text")).toContainText("Search failed");

    const retry = notice.locator("button[data-retry]");
    await expect(retry).toBeEnabled();
    await expect(retry).toHaveText(/try again/i);
    // A control you can actually press: at least a 40px tap target at every width.
    const box = await retry.boundingBox();
    expect(box?.height ?? 0).toBeGreaterThanOrEqual(40);

    // The retry is what proves the request is re-issued, so count the requests.
    let requests = 0;
    await page.route("**/api/v1/search*", async (route) => {
      requests += 1;
      await route.fulfill({
        status: 200,
        contentType: "application/json",
        body: JSON.stringify([
          {
            id: "b3b8b0f4-0b3e-4a5f-9c1e-1f4f2a6d1c11",
            title: "Kumquat tasting",
            snippet: "kumquat, candied",
            tag_id: "6a1a3f6e-5b1b-4a2f-8d3e-6f9c1b2a4d55",
            tag_name: "food",
          },
        ]),
      });
    });

    await retry.click();
    await expect(sheet.locator(".load-error")).toHaveCount(0);
    await expect(sheet.locator(".sheet__results")).toContainText("Kumquat tasting");
    expect(requests).toBeGreaterThan(0);

    await awaitSplashGone(page);
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
