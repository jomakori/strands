import { expect, test } from "@playwright/test";

// Runs against the release Wasm bundle CI just built (see home.spec.ts). Covers T6: opening
// a tag's detail view from the grid, the card fields, and the collapsible path stack.
test.describe("tag detail", () => {
  test("opens a tag card from the grid and shows its fields", async ({
    page,
  }, testInfo) => {
    await page.goto("/");

    const firstTag = page.locator("#grid [data-tag-id]").first();
    const tagId = await firstTag.getAttribute("data-tag-id");
    await firstTag.click();

    await expect(page).toHaveURL(new RegExp(`/tags/${tagId}$`));

    const card = page.locator("#tag-detail");
    await expect(card).toBeVisible();
    await expect(page.locator("#detail-name")).not.toBeEmpty();
    await expect(page.locator(".detail-category")).toBeVisible();
    await expect(page.locator(".detail-count")).toContainText(/thread/);
    await expect(page.locator(".detail-age")).toContainText("Active");

    const pathStack = page.locator("#path-stack");
    await expect(pathStack).toBeVisible();

    const entries = page.locator("[data-path-entry]");
    await expect(entries.first()).toBeVisible();
    await expect(page.locator("[data-path-segments]").first()).toBeVisible();

    await page.screenshot({
      path: `captures/${testInfo.project.name}-tag-detail.png`,
      fullPage: true,
    });

    // back navigation (browser history, driven by dioxus-router) returns to the grid
    await page.goBack();
    await expect(page.locator("#grid [data-tag-id]")).toHaveCount(16);
  });

  test("a path entry collapses and expands independently", async ({
    page,
  }) => {
    await page.goto("/");
    await page.locator("#grid [data-tag-id]").first().click();

    const firstSummary = page.locator("[data-path-entry]").first().locator(".path-summary");
    await expect(firstSummary).toHaveAttribute("aria-expanded", "true");
    await expect(page.locator("[data-path-segments]").first()).toBeVisible();

    await firstSummary.click();
    await expect(firstSummary).toHaveAttribute("aria-expanded", "false");
    await expect(page.locator("[data-path-segments]")).toHaveCount(
      (await page.locator("[data-path-entry]").count()) - 1,
    );
  });
});
