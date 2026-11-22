import type { Page } from "@playwright/test";
import { expect, test } from "@playwright/test";

import { awaitSplashGone } from "../support/splash";

const PROFILE = {
  id: "9f3d2a1e-0000-4000-8000-00000000a001",
  email: "alice@example.com",
  name: "Alice",
  avatar_url: null,
  provider: "google",
};

const CONNECTED = {
  id: "google-photos",
  label: "Google Photos",
  scopes: ["photoslibrary.readonly"],
  status: "connected",
  error: null,
};

/** Only the two routes this spec reads are stubbed; the rest reaches the fixture server. */
async function stubConnectors(page: Page) {
  await page.route("**/api/v1/**", async (route) => {
    const path = new URL(route.request().url()).pathname;
    if (path.endsWith("/me")) {
      return route.fulfill({
        status: 200,
        contentType: "application/json",
        body: JSON.stringify(PROFILE),
      });
    }
    if (path.endsWith("/connectors")) {
      return route.fulfill({
        status: 200,
        contentType: "application/json",
        body: JSON.stringify([CONNECTED]),
      });
    }
    return route.continue();
  });
}

// Screens that drill carry the mockup's head: back crumb, the title of where you are, close.
test.describe("screen navigation", () => {
  test("a connector row drills in and comes back", async ({
    page,
  }, testInfo) => {
    await stubConnectors(page);
    await page.goto("/settings");

    const row = page.locator(".connector-open").first();
    await expect(row).toBeVisible();
    await expect(row.locator(".connector-act")).toHaveText(/Connect|Manage/);

    await row.click();
    await expect(page).toHaveURL(/\/connectors\/[a-z0-9_-]+$/);
    await expect(page.locator("#connector-title")).toBeVisible();
    await expect(page.locator("#settings-title")).toHaveCount(0);

    await awaitSplashGone(page);
    await page.screenshot({
      path: `captures/${testInfo.project.name}-connector-drill.png`,
      fullPage: true,
    });

    await page.locator(".screen-head .back").click();
    await expect(page).toHaveURL(/\/settings$/);
    await expect(page.locator("#settings-title")).toHaveText("Connectors");
    await expect(page.locator(".screen-head .back")).toHaveCount(0);
  });

  test("the tag screen names its parent and closes home", async ({
    page,
  }) => {
    await page.goto("/");
    await page.locator("a[href^='/tags/']").first().click();
    await expect(page).toHaveURL(/\/tags\//);

    const back = page.locator(".screen-head .back");
    await expect(back).toHaveText(/Tags/);
    await expect(page.locator(".screen-title")).not.toBeEmpty();

    await page.locator(".screen-head .icon-btn[aria-label='Close']").click();
    await expect(page).toHaveURL(/\/$/);
  });
});
