import type { Page } from "@playwright/test";
import { expect, test } from "@playwright/test";

// Runs against the release Wasm bundle CI just built, with the API stubbed at the
// network boundary the way `account.spec.ts` and `capture.spec.ts` do: the visual
// job serves the bundle statically with no database behind it, so this file owns
// the render path while the rows themselves — stored tokens, sealing keys, provider
// failures — are covered against a real Postgres in
// `crates/server/tests/connector_registry.rs`.
//
// T27 acceptance: the row states the ticket names (NeedsSignIn → connected → a
// visible provider failure) must be reachable by driving the UI, not only by
// calling the route.
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

const NEEDS_SIGN_IN = {
  id: "gmail",
  label: "Gmail",
  scopes: ["gmail.readonly"],
  status: "needs_sign_in",
  error: null,
};

/// The shape `GET /api/v1/connectors` produces when a connector's own status check
/// blew up (`connectors/mod.rs::connector_status` maps the error to `status: null`
/// plus the message). This text is the one the deployed pod logged on first boot,
/// verbatim from `crates/server/src/connectors/photos.rs`.
const FAILING = {
  id: "google-photos",
  label: "Google Photos",
  scopes: ["photoslibrary.readonly"],
  status: null,
  error:
    "STRANDS_SECRET_KEY is not set, so stored Google Photos tokens cannot be opened",
};

/** Settings is reached the way a user reaches it: the `,` binding, after signing in. */
async function openSettings(page: Page) {
  await page.goto("/");
  // Hydration: the shortcut listener is only live once the signed-in shell renders.
  await expect(page.locator("#account-link")).toBeVisible();
  await page.keyboard.press(",");
  await expect(page).toHaveURL(/\/settings$/);
}

test.describe("connector settings rows", () => {
  test("shows needs-sign-in, connected and failing rows side by side", async ({
    page,
  }) => {
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
          body: JSON.stringify([CONNECTED, NEEDS_SIGN_IN]),
        });
      }
      return route.fulfill({
        status: 200,
        contentType: "application/json",
        body: "[]",
      });
    });

    await openSettings(page);

    await expect(page.locator("#settings-title")).toHaveText("Connectors");

    const photos = page.locator('[data-connector-id="google-photos"]');
    await expect(photos.locator(".connector-label")).toHaveText("Google Photos");
    await expect(photos.locator(".status-chip")).toHaveAttribute(
      "data-status",
      "connected",
    );
    await expect(photos.locator(".connector-scope")).toHaveText([
      "photoslibrary.readonly",
    ]);

    const gmail = page.locator('[data-connector-id="gmail"]');
    await expect(gmail.locator(".status-chip")).toHaveAttribute(
      "data-status",
      "needs-sign-in",
    );
    await expect(gmail.locator(".status-chip")).toHaveText("Needs sign-in");
    await expect(gmail.locator(".connector-scope")).toHaveText([
      "gmail.readonly",
    ]);
  });

  test("a connector whose status check failed shows the reason and keeps its row", async ({
    page,
  }) => {
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
          body: JSON.stringify([FAILING, NEEDS_SIGN_IN]),
        });
      }
      return route.fulfill({
        status: 200,
        contentType: "application/json",
        body: "[]",
      });
    });

    await openSettings(page);

    const failing = page.locator('[data-connector-id="google-photos"]');
    await expect(failing.locator(".status-chip")).toHaveAttribute(
      "data-status",
      "error",
    );
    await expect(failing.locator("[data-connector-error]")).toHaveText(
      FAILING.error,
    );

    // The neighbour is unaffected and the list did not collapse to one error.
    await expect(
      page.locator('[data-connector-id="gmail"] .status-chip'),
    ).toHaveAttribute("data-status", "needs-sign-in");
  });

  test("an unconfigured server says so instead of showing a blank list", async ({
    page,
  }) => {
    await page.route("**/api/v1/**", async (route) => {
      const path = new URL(route.request().url()).pathname;
      if (path.endsWith("/me")) {
        return route.fulfill({
          status: 200,
          contentType: "application/json",
          body: JSON.stringify(PROFILE),
        });
      }
      // The honest production answer before any connector is configured: an
      // empty registry, which the screen has to name rather than render emptily.
      return route.fulfill({
        status: 200,
        contentType: "application/json",
        body: "[]",
      });
    });

    await openSettings(page);

    await expect(page.locator("[data-connector-id]")).toHaveCount(0);
    await expect(page.locator(".empty-notice")).toContainText(
      "No connectors registered",
    );
  });
});
