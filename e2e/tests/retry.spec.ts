import { expect, test, type Page } from "@playwright/test";

import { PROFILE } from "../support/session";

import { awaitSplashGone } from "../support/splash";

// Ticket T18. Every fetch that can fail has to hand the request back to the reader: a
// notice that names what broke, a control that re-issues the same call, and no state
// where the page only says "something went wrong". These specs stub the API at the
// network boundary and then count the requests the app makes, so "the retry works" is
// measured as a second request on the wire, not as a button that merely exists.
//
// One call is not retryable from inside the app: `GET /api/v1/me` above the router decides
// whether there is a session at all (T24), and a failure there shows the gate. Every blanket
// stub below therefore answers `/me` itself, and the account test fails only the call the
// screen makes after the app is already open.

/** Stub one endpoint to fail every time, and count the requests it received. */
async function failing(page: Page, pattern: string) {
  const calls: string[] = [];
  await page.route(pattern, async (route) => {
    calls.push(`${route.request().method()} ${new URL(route.request().url()).pathname}`);
    await route.fulfill({ status: 500, contentType: "text/plain", body: "boom" });
  });
  return calls;
}

/** The notice for a failed fetch, with the control that asks again. */
async function retryControl(page: Page, root = ".load-error") {
  const notice = page.locator(root).first();
  await expect(notice).toBeVisible();
  const button = notice.locator('button[data-retry="true"]');
  await expect(button).toHaveCount(1);
  await expect(button).toBeEnabled();
  await expect(button).toHaveText(/try again/i);
  return button;
}

test.describe("retry affordance", () => {
  test("account: a failed profile load re-issues /me when retried", async ({
    page,
    browserName,
  }, testInfo) => {
    void browserName;
    // The gate's probe has to answer, or the app never opens; the account screen's call is the
    // one that fails, and that is the one this test retries.
    const calls: string[] = [];
    let answered = 0;
    await page.route("**/api/v1/me", async (route) => {
      calls.push(`${route.request().method()} ${new URL(route.request().url()).pathname}`);
      answered += 1;
      if (answered === 1) {
        return route.fulfill({
          status: 200,
          contentType: "application/json",
          body: JSON.stringify(PROFILE),
        });
      }
      return route.fulfill({ status: 500, contentType: "text/plain", body: "boom" });
    });
    await page.route("**/api/v1/tags", (route) =>
      route.fulfill({ status: 200, contentType: "application/json", body: "[]" }),
    );
    await page.route("**/api/v1/categories", (route) =>
      route.fulfill({ status: 200, contentType: "application/json", body: "[]" }),
    );

    await page.goto("/");
    // The grid read is stubbed to nothing above, so the surface is up once it has answered —
    // an empty grid, not a sample one (T25). The account link is the shell's own footer.
    await expect(page.locator("#account-link")).toBeVisible();
    await page.locator("#account-link").click();
    await expect(page).toHaveURL(/\/account$/);

    const button = await retryControl(page);
    const before = calls.length;
    expect(before).toBeGreaterThan(0);

    await button.click();
    await expect.poll(() => calls.length).toBeGreaterThan(before);

    await awaitSplashGone(page);
    await page.screenshot({
      path: `captures/${testInfo.project.name}-retry-account.png`,
      fullPage: false,
    });
  });

  test("tag card: a failed summary history re-issues the request", async ({
    page,
  }, testInfo) => {
    const calls = await failing(page, "**/api/v1/tags/*/summary/history");

    await page.goto("/");
    const firstTag = page.locator("#grid [data-tag-id]").first();
    await expect(firstTag).toBeVisible();
    await firstTag.click();
    await expect(page.locator("#tag-detail")).toBeVisible();

    const button = await retryControl(page);
    const before = calls.length;
    expect(before).toBeGreaterThan(0);

    await button.click();
    await expect.poll(() => calls.length).toBeGreaterThan(before);

    await awaitSplashGone(page);
    await page.screenshot({
      path: `captures/${testInfo.project.name}-retry-tag-card.png`,
    });
  });

  test("ingest: a failed save re-submits the same note", async ({ page }, testInfo) => {
    const calls = await failing(page, "**/api/v1/threads");
    await page.route("**/api/v1/categories", (route) =>
      route.fulfill({
        status: 200,
        contentType: "application/json",
        body: JSON.stringify([
          {
            id: "9f3d2a1e-0000-4000-8000-0000000000c1",
            name: "Travel",
            color: "#6aa9ff",
            position: 0,
          },
        ]),
      }),
    );
    // Creating the tag has to succeed, or the form never gets a tag id to save against.
    await page.route("**/api/v1/tags", async (route) => {
      const created =
        route.request().method() === "POST"
          ? {
              id: "9f3d2a1e-0000-4000-8000-000000000001",
              name: "Sapporo",
              category_id: "9f3d2a1e-0000-4000-8000-0000000000c1",
              thread_count: 0,
              created_at: "2026-11-15T12:00:00Z",
            }
          : [];
      await route.fulfill({
        status: 200,
        contentType: "application/json",
        body: JSON.stringify(created),
      });
    });

    await page.goto("/");
    await page.locator('[data-testid="capture-open"]').click();
    await expect(page.locator("#capture-body")).toBeVisible();

    await page.locator("#capture-body").fill("A note the server refuses");
    // A new tag is what makes the form ready; the type-ahead needs real keystrokes.
    await page
      .locator('[data-testid="capture-tag"]')
      .pressSequentially("Sapporo", { delay: 20 });
    await page.locator('[data-testid="capture-create"]').waitFor();
    await page.locator('[data-cat="travel"]').click();

    await page.locator("#capture-save").click();

    const error = page.locator("#capture-error");
    await expect(error).toBeVisible();
    await expect(error).toHaveAttribute("role", "alert");
    const button = error.locator("#capture-retry");
    await expect(button).toBeEnabled();

    const before = calls.length;
    expect(before).toBeGreaterThan(0);

    await button.click();
    await expect.poll(() => calls.length).toBeGreaterThan(before);

    await awaitSplashGone(page);
    await page.screenshot({
      path: `captures/${testInfo.project.name}-retry-ingest.png`,
    });
  });

  test("ingest: a failed tag picker re-issues the tag list", async ({ page }) => {
    const calls = await failing(page, "**/api/v1/tags");
    await page.route("**/api/v1/categories", (route) =>
      route.fulfill({ status: 200, contentType: "application/json", body: "[]" }),
    );

    await page.goto("/");
    await page.locator('[data-testid="capture-open"]').click();
    await expect(page.locator("#capture-body")).toBeVisible();

    const button = await retryControl(page);
    const before = calls.length;
    expect(before).toBeGreaterThan(0);

    await button.click();
    await expect.poll(() => calls.length).toBeGreaterThan(before);
  });

  test("constellation: a failed load re-issues it", async ({ page }) => {
    await page.route("**/api/v1/tags/*/links", (route) =>
      route.fulfill({ status: 200, contentType: "application/json", body: "[]" }),
    );
    const calls = await failing(page, "**/api/v1/tags");

    await page.goto("/");
    await expect(page.locator("#constellation-link")).toBeVisible();
    await page.locator("#constellation-link").click();
    await expect(page).toHaveURL(/\/constellation$/);

    const button = await retryControl(page);
    const before = calls.length;
    expect(before).toBeGreaterThan(0);

    await button.click();
    await expect.poll(() => calls.length).toBeGreaterThan(before);
  });

  test("search: a failed lookup re-issues the query", async ({ page }) => {
    const calls = await failing(page, "**/api/v1/search*");

    await page.goto("/");
    await expect(page.locator("#grid [data-tag-id]").first()).toBeVisible();
    await page.keyboard.press("/");
    await expect(page.locator("#search-sheet")).toBeVisible();

    // Recents are a lookup too: the sheet must not sit on a bare "failed" line.
    const button = await retryControl(page, "#search-sheet .load-error");
    const before = calls.length;
    expect(before).toBeGreaterThan(0);

    await button.click();
    await expect.poll(() => calls.length).toBeGreaterThan(before);
  });

  test("no failure notice is a dead end", async ({ page }) => {
    // Everything the settings screen and the shell ask for fails at once — except the session
    // probe, which is what decides whether there is an app to show (T24).
    await page.route("**/api/v1/**", (route) => {
      if (new URL(route.request().url()).pathname.endsWith("/me")) {
        return route.fulfill({
          status: 200,
          contentType: "application/json",
          body: JSON.stringify(PROFILE),
        });
      }
      return route.fulfill({ status: 500, contentType: "text/plain", body: "boom" });
    });

    await page.goto("/");
    // Every read fails here, the grid's included: the surface is up once the failure has been
    // rendered, not when rows exist (T25 moved the grid onto the server's answer).
    await expect(page.locator("#account-link")).toBeVisible();
    await page.keyboard.press(",");
    await expect(page).toHaveURL(/\/settings$/);

    const notices = page.locator(".load-error");
    await expect(notices.first()).toBeVisible();
    const count = await notices.count();
    for (let index = 0; index < count; index += 1) {
      await expect(
        notices.nth(index).locator('button[data-retry="true"]'),
      ).toHaveCount(1);
    }
  });
});
