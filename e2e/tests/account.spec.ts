import { expect, test } from "@playwright/test";

// Runs against the release Wasm bundle CI just built. The API is stubbed at the network
// boundary, exactly like `capture.spec.ts`: the visual job serves the bundle statically with
// no database behind it, so this test owns the render path while the export/delete endpoints
// themselves are covered by `crates/server/tests/account.rs`.
const PROFILE = {
  id: "9f3d2a1e-0000-4000-8000-00000000a001",
  email: "alice@example.com",
  name: "Alice",
  avatar_url: null,
  provider: "google",
};

test.describe("account", () => {
  test("shows the signed-in identity and the export control", async ({
    page,
  }, testInfo) => {
    await page.route("**/api/v1/**", async (route) => {
      const path = new URL(route.request().url()).pathname;
      const json = (body: unknown) =>
        route.fulfill({
          status: 200,
          contentType: "application/json",
          body: JSON.stringify(body),
        });

      if (path.endsWith("/categories")) return json([]);
      if (path.endsWith("/me")) return json(PROFILE);
      if (path.endsWith("/tags")) return json([]);
      return route.fulfill({ status: 404, body: "unexpected request" });
    });

    // Client-side navigation: the static bundle server has no `/account` file.
    await page.goto("/");
    await page.locator("#account-link").click();
    await expect(page).toHaveURL(/\/account$/);

    await expect(page.locator("#account-title")).toHaveText("Account");
    await expect(page.locator("#account-email")).toHaveText(PROFILE.email);
    await expect(page.locator("#account-name")).toHaveText(PROFILE.name);
    await expect(page.locator("#account-provider")).toHaveText("google");

    const exportControl = page.locator("#account-export");
    await expect(exportControl).toBeVisible();
    await expect(exportControl).toHaveAttribute("href", "/api/v1/account/export");
    await expect(exportControl).toHaveAttribute("download", /\.json$/);

    // Deleting is two-step: the destructive press only reveals the confirm step.
    await page.locator("#account-delete").click();
    await expect(page.locator("#account-delete-confirm")).toBeVisible();
    await expect(page.locator("#account-confirm-text")).toContainText(
      "cannot be undone",
    );
    await page.locator("#account-delete-cancel").click();
    await expect(page.locator("#account-delete")).toBeVisible();

    // Usable at 360px: nothing may push the page wider than the viewport.
    const { clientWidth, scrollWidth } = await page.evaluate(() => ({
      clientWidth: document.documentElement.clientWidth,
      scrollWidth: document.documentElement.scrollWidth,
    }));
    expect(clientWidth).toBe(360);
    expect(
      scrollWidth,
      `account screen scrolls horizontally: content is ${scrollWidth}px wide in a ${clientWidth}px viewport`,
    ).toBeLessThanOrEqual(clientWidth);

    await page.screenshot({
      path: `captures/${testInfo.project.name}-account.png`,
      fullPage: true,
    });
  });
});
