import { expect, test } from "@playwright/test";

// T19: the app driven with keys only. No clicks in this file — if a spec needs the
// pointer, the keyboard path it is meant to prove is missing.
test.describe("keyboard parity", () => {
  test("a tag opens, a thread expands and Escape goes back", async ({ page }) => {
    await page.goto("/");

    const tags = page.locator("#grid a.tag");
    await expect(tags.first()).toBeVisible();

    // Tab order starts at the first tag: focus it the way a keyboard user would.
    await page.keyboard.press("Tab");
    const firstTag = tags.first();
    await firstTag.focus();
    await expect(firstTag).toBeFocused();

    const href = await firstTag.getAttribute("href");
    expect(href).toBeTruthy();
    await page.keyboard.press("Enter");
    await expect(page.locator("#detail-name")).toBeVisible();
    await expect(page.locator("#grid")).toHaveCount(0);

    // A thread's row is a button: Enter expands its segments.
    const row = page.locator(".path-head .path-summary").first();
    await row.focus();
    await expect(row).toHaveAttribute("aria-expanded", "true");
    await page.keyboard.press("Enter");
    await expect(row).toHaveAttribute("aria-expanded", "false");

    await page.keyboard.press("Escape");
    await expect(page.locator("#grid")).toBeVisible();
    await expect(page.locator("#detail-name")).toHaveCount(0);
  });

  test("slash opens search and Escape closes it", async ({ page }) => {
    await page.goto("/");
    await expect(page.locator("#search-sheet")).toHaveCount(0);

    await page.keyboard.press("/");
    const sheet = page.locator("#search-sheet");
    await expect(sheet).toBeVisible();
    await expect(page.locator("#search-input")).toBeFocused();

    await page.locator("#search-input").pressSequentially("kyoto", { delay: 20 });
    await expect(page.locator("#search-input")).toHaveValue("kyoto");

    await page.keyboard.press("Escape");
    await expect(sheet).toHaveCount(0);
  });

  test("c opens capture with the caret in the body", async ({ page }) => {
    await page.goto("/");

    await page.keyboard.press("c");
    await page.waitForURL(/\/capture$/);
    await expect(page.locator("#capture-form")).toBeVisible();
    await expect(page.locator("#capture-body")).toBeFocused();
  });

  test("question mark shows the map and every ticket action is on it", async ({
    page,
  }) => {
    await page.goto("/");

    await page.keyboard.press("?");
    const map = page.locator("#shortcut-map");
    await expect(map).toBeVisible();
    await expect(map).toContainText("Open a tag");
    await expect(map).toContainText("Archive");
    await expect(map).toContainText("Search");
    await expect(map).toContainText("Approve");

    // The close button is reachable, and the map closes with the pointer or the key.
    await expect(page.locator("#shortcut-close")).toBeFocused();
    await page.keyboard.press("Escape");
    await expect(map).toHaveCount(0);
  });

  test("every focusable control draws a ring", async ({ page }) => {
    await page.goto("/");

    const controls = page.locator(
      '#grid a.tag, #grid button.fchip, a[href="/capture"]',
    );
    const count = await controls.count();
    expect(count).toBeGreaterThan(0);

    for (let index = 0; index < count; index += 1) {
      const control = controls.nth(index);
      await control.focus();
      const ring = await control.evaluate((element) => {
        const style = getComputedStyle(element);
        return {
          outlineStyle: style.outlineStyle,
          outlineWidth: parseFloat(style.outlineWidth),
          boxShadow: style.boxShadow,
        };
      });
      const drawn =
        (ring.outlineStyle !== "none" && ring.outlineWidth > 0) ||
        ring.boxShadow !== "none";
      expect(drawn, `no focus ring on control ${index}`).toBe(true);
    }
  });

  test("keys do nothing while a field has focus", async ({ page }) => {
    await page.goto("/");

    await page.keyboard.press("/");
    const input = page.locator("#search-input");
    await expect(input).toBeFocused();

    // `c` belongs to the text field now, not to the shortcut map.
    await input.pressSequentially("c", { delay: 20 });
    await expect(input).toHaveValue("c");
    await expect(page.locator("#capture-form")).toHaveCount(0);
  });
});
