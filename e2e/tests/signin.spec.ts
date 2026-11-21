import { expect, test, type Browser, type TestInfo } from "@playwright/test";

import {
  PROFILE,
  readGateWatch,
  sessionCookie,
  signedIn,
  signedOut,
  watchGate,
} from "../support/session";

import { awaitSplashGone } from "../support/splash";

// Ticket T24. The app is behind a sign-in gate, and the gate is decided by one answer from the
// API: `GET /api/v1/me`. These specs drive a real bundle in a real browser against `serve.py`,
// which stands in for that one endpoint, so what is asserted is the render path the shipped
// Wasm takes — not a component in isolation.
//
// Four claims, each with its own test:
//
//   1. no session → the gate, and nothing from behind it;
//   2. a live session → the app, and a reload never flashes the gate;
//   3. a 401 while in use → back to the gate, not an error panel;
//   4. a deep link → the gate, then the route the visitor asked for.
//
// Every context here starts empty (`signedOut` below), which is the opposite of the suite
// default, so "signed in" has to be spelled out per test.

test.use({ storageState: signedOut });

/** A context holding a session cookie for as long as the test needs it. */
async function withSession(
  browser: Browser,
  testInfo: TestInfo,
  cookieValue: string = sessionCookie.value,
) {
  const context = await browser.newContext({
    storageState: { cookies: [{ ...sessionCookie, value: cookieValue }], origins: [] },
    baseURL: testInfo.project.use.baseURL,
  });
  return { context, page: await context.newPage() };
}

/** Everything the app proper would put on screen. None of it may render before a session. */
const BEHIND_THE_GATE = [
  "#grid",
  "#feed-count",
  "#account-link",
  "#constellation-link",
  "#settings-title",
  ".fchip",
];

async function expectGateOnly(page: import("@playwright/test").Page) {
  await expect(page.locator("#gate")).toBeVisible();
  await expect(page.locator("#sign-in")).toBeVisible();
  for (const selector of BEHIND_THE_GATE) {
    await expect(page.locator(selector), `${selector} rendered before a session was confirmed`).toHaveCount(0);
  }
  // A gate is not an error state: it must not carry a failed-fetch notice either.
  await expect(page.locator(".load-error")).toHaveCount(0);
}

test.describe("sign-in gate", () => {
  test("a browser with no session gets the gate and nothing behind it", async ({
    page,
  }, testInfo) => {
    await watchGate(page);
    await page.goto("/");

    await expectGateOnly(page);
    // The root is the only route that carries no `next`: the server defaults there.
    await expect(page.locator("#sign-in")).toHaveAttribute("href", "/auth/google");
    // One way in, and it is a link out to the server's OIDC start — not a form.
    await expect(page.locator("#gate a, #gate button")).toHaveCount(1);

    // The watcher is what test 2 uses to say "the gate never appeared"; prove it can see a
    // gate, or that assertion would be vacuous.
    expect((await readGateWatch(page)).gate).toBe(true);

    await awaitSplashGone(page);
    await page.screenshot({
      path: `captures/${testInfo.project.name}-signin-gate.png`,
      fullPage: false,
    });
  });

  test("a live session opens the app, and a reload never flashes the gate", async ({
    browser,
  }, testInfo) => {
    const { context, page } = await withSession(browser, testInfo);
    // Installed before the first navigation, so the tally covers the whole document.
    await watchGate(page);

    await page.goto("/");
    await expect(page.locator("#grid [data-tag-id]")).toHaveCount(16);
    await expect(page.locator("#gate")).toHaveCount(0);

    const loaded = await readGateWatch(page);
    expect(loaded.probing, "the probe frame never rendered — the watch is not working").toBe(true);
    expect(loaded.gate, "the gate reached the DOM on a signed-in load").toBe(false);

    // The claim that matters: a reload with a session in hand goes straight to the app.
    await page.reload();
    await expect(page.locator("#grid [data-tag-id]")).toHaveCount(16);
    const reloaded = await readGateWatch(page);
    expect(reloaded.probing).toBe(true);
    expect(reloaded.gate, "the gate flashed on a signed-in reload").toBe(false);
    expect(reloaded.frames).toBeGreaterThan(0);
    await expect(page.locator("#sign-in")).toHaveCount(0);
    await expect(page.locator("#feed-count")).toBeVisible();

    await context.close();
  });

  test("a cookie the server never issued still lands on the gate", async ({
    browser,
  }, testInfo) => {
    // The app must not treat "a cookie exists" as a session: only `/api/v1/me` decides.
    const { context, page } = await withSession(browser, testInfo, "not-a-session");
    await page.goto("/");

    await expectGateOnly(page);
    await context.close();
  });

  test("the API decides, not the cookie jar", async ({ page }) => {
    // No cookie at all, and the API says this visitor has a session: the app must believe the
    // API. Together with the test above this is the whole claim — nothing here reads a cookie,
    // and nothing reads a token; `api::me` is the single source of truth (T24).
    //
    // The reads are answered here too, because the grid is a server read (T25): without them the
    // tag call would 401 and the gate — not a grid — would be the correct screen.
    await page.route("**/api/v1/**", (route) => {
      const path = new URL(route.request().url()).pathname;
      const body = path.endsWith("/me")
        ? PROFILE
        : path.endsWith("/categories")
          ? [
              {
                id: "0c9a2d8e-0000-4000-8000-0000000000c1",
                name: "Travel",
                color: "#2F6F4F",
                position: 0,
              },
            ]
          : path.endsWith("/tags")
            ? [
                {
                  id: "0c9a2d8e-0000-4000-8000-000000000001",
                  name: "Tokyo",
                  category_id: "0c9a2d8e-0000-4000-8000-0000000000c1",
                  thread_count: 0,
                  created_at: "2026-11-15T12:00:00Z",
                },
              ]
            : [];
      return route.fulfill({
        status: 200,
        contentType: "application/json",
        body: JSON.stringify(body),
      });
    });
    await page.goto("/");

    await expect(page.locator("#grid [data-tag-id]")).toHaveCount(1);
    await expect(page.locator("#grid")).toContainText("Tokyo");
    await expect(page.locator("#gate")).toHaveCount(0);
  });

  test("a 401 mid-use returns to the gate instead of an error panel", async ({
    browser,
  }, testInfo) => {
    const { context, page } = await withSession(browser, testInfo);
    await page.goto("/");
    await expect(page.locator("#grid [data-tag-id]")).toHaveCount(16);

    // The session dies while the app is open: the next call is answered 401.
    await page.route("**/api/v1/me", (route) =>
      route.fulfill({
        status: 401,
        contentType: "application/json",
        body: JSON.stringify({ error: "unauthenticated" }),
      }),
    );

    await page.locator("#account-link").click();
    await expect(page).toHaveURL(/\/account$/);

    // The screen the visitor asked for is replaced by the gate, with the deep link intact.
    await expectGateOnly(page);
    await expect(page.locator("#sign-in")).toHaveAttribute("href", "/auth/google?next=/account");
    await expect(page.locator("#account-title")).toHaveCount(0);

    await context.close();
  });

  test("a deep link survives sign-in and lands on the route asked for", async ({
    page,
  }) => {
    await page.goto("/account");

    await expectGateOnly(page);
    await expect(page.locator("#sign-in")).toHaveAttribute("href", "/auth/google?next=/account");

    // Leaves for the server's OIDC start; `serve.py` ends the round trip the way the real
    // callback does, with a session cookie and a redirect back to `next`.
    await page.locator("#sign-in").click();
    await expect(page).toHaveURL(/\/account$/);

    await expect(page.locator("#gate")).toHaveCount(0);
    await expect(page.locator("#account-title")).toBeVisible();
    // Signed in for real, as far as the app knows: the identity is the API's answer.
    await expect(page.locator("#account-email")).toHaveText(PROFILE.email);
  });

  test("the gate fits the first screen at this viewport", async ({ page }, testInfo) => {
    // Runs at 360x800 and 1440x1000 — the two projects. One column, one control, no scrolling.
    await page.goto("/");
    await expect(page.locator("#gate")).toBeVisible();

    const viewport = page.viewportSize();
    expect(viewport).not.toBeNull();
    const cta = await page.locator("#sign-in").boundingBox();
    expect(cta).not.toBeNull();
    if (cta === null || viewport === null) return;

    // A comfortable touch target, fully on screen.
    expect(cta.height).toBeGreaterThanOrEqual(44);
    expect(cta.x).toBeGreaterThanOrEqual(0);
    expect(cta.x + cta.width).toBeLessThanOrEqual(viewport.width);

    const layout = await page.evaluate(() => ({
      clientWidth: document.documentElement.clientWidth,
      scrollWidth: document.documentElement.scrollWidth,
      scrollHeight: document.documentElement.scrollHeight,
      innerHeight: window.innerHeight,
    }));
    expect(
      layout.scrollWidth,
      `the gate scrolls horizontally: ${layout.scrollWidth}px of content in ${layout.clientWidth}px`,
    ).toBeLessThanOrEqual(layout.clientWidth);
    expect(
      layout.scrollHeight,
      "the gate does not fit the first screen without scrolling",
    ).toBeLessThanOrEqual(layout.innerHeight + 1);

    await awaitSplashGone(page);
    await page.screenshot({
      path: `captures/${testInfo.project.name}-signin-gate-viewport.png`,
      fullPage: false,
    });
  });
});
