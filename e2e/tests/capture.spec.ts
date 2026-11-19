import { expect, test } from "@playwright/test";

import { PROFILE } from "../support/session";

// Runs against the release Wasm bundle CI just built. The API is stubbed at the network
// boundary: the visual job serves the bundle statically, with no database behind it, so the
// browser test owns the render path (capture → optimistic row → reconciled row) while the
// write path itself is covered by the server's own tests (crates/server/tests/capture.rs).
const TAG_ID = "9f3d2a1e-0000-4000-8000-000000000001";
const CATEGORY_ID = "9f3d2a1e-0000-4000-8000-0000000000c1";
const THREAD_ID = "9f3d2a1e-0000-4000-8000-0000000000f1";
const NOTE = "The 6 a.m. train is not what the guidebooks promise";

test.describe("quick capture", () => {
  test("a note typed on the capture screen lands in the grid", async ({
    page,
  }, testInfo) => {
    await page.route("**/api/v1/**", async (route) => {
      const request = route.request();
      const path = new URL(request.url()).pathname;
      const json = (body: unknown) =>
        route.fulfill({
          status: 200,
          contentType: "application/json",
          body: JSON.stringify(body),
        });

      if (path.endsWith("/me")) {
        // The session probe the gate makes before anything else renders (T24).
        return json(PROFILE);
      }
      if (path.endsWith("/categories")) {
        return json([
          { id: CATEGORY_ID, name: "Travel", color: "#6aa9ff", position: 0 },
        ]);
      }
      if (path.endsWith("/tags") && request.method() === "GET") {
        return json([]);
      }
      if (path.endsWith("/tags") && request.method() === "POST") {
        return json({
          id: TAG_ID,
          name: "Sapporo",
          category_id: CATEGORY_ID,
          thread_count: 0,
          created_at: "2026-11-15T12:00:00Z",
        });
      }
      if (path.endsWith("/threads") && request.method() === "POST") {
        return json({
          id: THREAD_ID,
          tag_id: TAG_ID,
          title: NOTE,
          source_count: 1,
          updated_at: "2026-11-15T12:00:00Z",
        });
      }
      return route.fulfill({ status: 404, body: "unexpected request" });
    });

    await page.goto("/");
    await page.locator('[data-testid="capture-open"]').click();
    await expect(page).toHaveURL(/\/capture$/);

    await page.locator('[data-testid="capture-body"]').fill(NOTE);
    await expect(page.locator("#capture-count")).toContainText(
      `${NOTE.length} / 5000`,
    );

    // A tag the caller does not have yet: the type-ahead offers to create it.
    // Typed keystroke by keystroke, not fill(): the tag box is a controlled input and a
    // synthetic single-shot input event does not reach Dioxus's handler.
    await page.locator('[data-testid="capture-tag"]').pressSequentially("Sapporo", { delay: 20 });
    await expect(page.locator('[data-testid="capture-create"]')).toBeVisible();
    await page.locator('[data-cat="travel"]').click();

    await page.locator('[data-testid="capture-save"]').click();

    // Back on the grid, the note is already visible inside the Sapporo card. The grid holds
    // exactly what this test's stub served — nothing — plus the row the server just accepted.
    await expect(page).toHaveURL(/\/$/);
    const created = page.locator(`[data-tag-id="${TAG_ID}"]`);
    await expect(created).toContainText(NOTE);
    await expect(page.locator("#grid [data-tag-id]")).toHaveCount(1);

    const { clientWidth, scrollWidth } = await page.evaluate(() => ({
      clientWidth: document.documentElement.clientWidth,
      scrollWidth: document.documentElement.scrollWidth,
    }));
    expect(
      scrollWidth,
      `page scrolls horizontally: content is ${scrollWidth}px wide in a ${clientWidth}px viewport`,
    ).toBeLessThanOrEqual(clientWidth);

    await page.screenshot({
      path: `captures/${testInfo.project.name}-quick-capture.png`,
      fullPage: true,
    });
  });
});
