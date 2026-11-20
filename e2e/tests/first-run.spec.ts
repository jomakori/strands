import { expect, test } from "@playwright/test";

import { awaitSplashGone } from "../support/splash";

// Ticket T29: the true first-run state — zero tags anywhere — points at importing from a
// connector instead of a blank capture form. This spec drives the board the way a brand-new
// account sees it (every read answered empty, home-server.spec.ts's seeded accounts never hit
// this path) and checks both ways out of it: the primary button to Settings (the connectors
// screen) and the quiet capture fallback that still exists for whoever would rather type.
test.describe("first-run board (T29)", () => {
  test("a brand-new account is told to import, not to capture", async ({
    page,
  }, testInfo) => {
    await page.route("**/api/v1/categories", (route) => route.fulfill({ json: [] }));
    await page.route("**/api/v1/tags", (route) => route.fulfill({ json: [] }));
    await page.route("**/api/v1/tags/*/threads", (route) => route.fulfill({ json: [] }));

    await page.goto("/");

    await expect(page.locator("#grid [data-tag-id]")).toHaveCount(0);
    const notice = page.locator('.empty-notice[data-state="empty"]');
    await expect(notice).toContainText("No strands yet");
    await expect(notice).toContainText(
      "Import from the apps you already write in — Gmail, Photos, WhatsApp, Maps.",
    );
    await expect(notice).toContainText("Every item you pick becomes a strand.");
    // The counts are the server's, same as every other read-backed state: zero rows is zero,
    // not a blank header.
    await expect(page.locator("#feed-count")).toHaveText("0 tags · 0 threads");

    // The primary action is the connectors screen, not the capture form — import-first.
    const importLink = page.locator('[data-testid="first-run-import"]');
    await expect(importLink).toHaveText("Import strands");
    await expect(importLink).toHaveAttribute("href", "/settings");

    // The quiet secondary path still reaches capture, for whoever would rather type a note.
    const captureLink = page.locator('[data-testid="first-run-capture"]');
    await expect(captureLink).toHaveText("or capture a note");
    await expect(captureLink).toHaveAttribute("href", "/capture");

    // Both ways out are touch targets: an anchor with a `min-height` but an inline box would
    // collapse to the height of its text, so the box itself is measured, not the padding.
    for (const [name, link] of [
      ["import", importLink],
      ["capture", captureLink],
    ] as const) {
      const box = await link.boundingBox();
      expect(box?.height ?? 0, `${name} target is under 44px tall`).toBeGreaterThanOrEqual(44);
    }

    await awaitSplashGone(page);
    await page.screenshot({
      path: `captures/${testInfo.project.name}-empty.png`,
      fullPage: true,
    });
  });

  test("the import link actually routes to Settings", async ({ page }) => {
    await page.route("**/api/v1/categories", (route) => route.fulfill({ json: [] }));
    await page.route("**/api/v1/tags", (route) => route.fulfill({ json: [] }));
    await page.route("**/api/v1/tags/*/threads", (route) => route.fulfill({ json: [] }));
    await page.route("**/api/v1/connectors", (route) => route.fulfill({ json: [] }));

    await page.goto("/");
    await page.locator('[data-testid="first-run-import"]').click();
    await expect(page).toHaveURL(/\/settings$/);
  });

  test("the quiet link actually routes to Capture", async ({ page }) => {
    await page.route("**/api/v1/categories", (route) => route.fulfill({ json: [] }));
    await page.route("**/api/v1/tags", (route) => route.fulfill({ json: [] }));
    await page.route("**/api/v1/tags/*/threads", (route) => route.fulfill({ json: [] }));

    await page.goto("/");
    await page.locator('[data-testid="first-run-capture"]').click();
    await expect(page).toHaveURL(/\/capture$/);
  });

  // The two empty states answer different questions, so they must not collapse into one: an
  // account with tags whose active chip hides them all still has somewhere for a note to land,
  // so it keeps the plain notice and the capture hint instead of the import pitch.
  test("a chip that hides every tag keeps the filtered-empty state", async ({ page }) => {
    const travel = "3f0c1f7a-0000-4000-8000-0000000000b1";
    const kyoto = "3f0c1f7a-0000-4000-8000-0000000000b2";
    await page.route("**/api/v1/categories", (route) =>
      route.fulfill({
        json: [
          { id: travel, name: "Travel", color: "#2F6F4F", position: 0 },
        ],
      }),
    );
    await page.route("**/api/v1/tags", (route) =>
      route.fulfill({
        json: [
          {
            id: kyoto,
            name: "Kyoto",
            category_id: travel,
            thread_count: 1,
            created_at: "2026-11-01T00:00:00Z",
          },
        ],
      }),
    );
    await page.route("**/api/v1/tags/*/threads", (route) => route.fulfill({ json: [] }));

    await page.goto("/");
    await expect(page.locator("#grid [data-tag-id]")).toHaveCount(1);

    // Tech holds no rows, so the visible set is empty while the account is not new.
    await page.locator('[data-chip="tech"]').click();

    const notice = page.locator('.empty-notice[data-state="empty"]');
    await expect(notice).toContainText("Nothing in these categories");
    await expect(notice).toContainText("Capture a note and it lands in a tag here.");
    await expect(notice).not.toContainText("No strands yet");
    await expect(page.locator('[data-testid="first-run-import"]')).toHaveCount(0);
    await expect(page.locator("#feed-count")).toHaveText("0 tags · 0 threads");
  });
});
