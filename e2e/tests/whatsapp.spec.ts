import type { Page } from "@playwright/test";
import { expect, test } from "@playwright/test";

import { awaitSplashGone } from "../support/splash";

// Runs against the release Wasm bundle with the API stubbed at the network
// boundary, like `settings.spec.ts`: the visual job has no database, so this file
// owns the render path while the transport — pairing, ingest, the purge — is
// covered against a real Postgres in `crates/server/tests/whatsapp_import.rs`.
//
// T28 acceptance: the Settings row shows a live link state, a dead bridge is
// visible with a retry, and the unofficial nature of the bridge is on the row
// itself rather than in a footnote elsewhere.
const PROFILE = {
  id: "9f3d2a1e-0000-4000-8000-00000000a001",
  email: "alice@example.com",
  name: "Alice",
  avatar_url: null,
  provider: "google",
};

/** The row is one of the connector summaries, so the list has to carry it. */
const WHATSAPP_ROW = {
  id: "whatsapp",
  label: "WhatsApp",
  scopes: [],
  status: "connected",
  error: null,
};

const CHATS = [
  {
    jid: "15551230001@s.whatsapp.net",
    display_name: "Sam",
    is_group: false,
    allowed: true,
    buffered_messages: 3,
  },
  {
    jid: "120363000000000001@g.us",
    display_name: "Studio",
    is_group: true,
    allowed: false,
    buffered_messages: 0,
  },
];

const LINKED = {
  state: { state: "linked" },
  disclosure: "Unofficial WhatsApp link. WhatsApp's Terms of Service do not cover this.",
  allowlist: ["15551230001@s.whatsapp.net"],
  chats: CHATS,
  last_sync_at: "2026-11-19T18:04:00Z",
  last_error: null,
};

const FAILED = {
  ...LINKED,
  state: { state: "failed", reason: "the phone unlinked this device" },
  chats: [],
  allowlist: [],
  last_error: "the phone unlinked this device",
};

/** Settings is reached the way a user reaches it: the `,` binding, after signing in. */
async function openSettings(page: Page) {
  await page.goto("/");
  await expect(page.locator("#account-link")).toBeVisible();
  await page.keyboard.press(",");
  await expect(page).toHaveURL(/\/settings$/);
}

const row = '[data-connector-id="whatsapp"]';

test.describe("whatsapp connector row", () => {
  test("a linked phone lists its chats and says the link is unofficial", async ({
    page,
  }, testInfo) => {
    await page.route("**/api/v1/**", async (route) => {
      const path = new URL(route.request().url()).pathname;
      if (path.endsWith("/me")) {
        return route.fulfill({
          status: 200,
          contentType: "application/json",
          body: JSON.stringify(PROFILE),
        });
      }
      if (path.endsWith("/whatsapp")) {
        return route.fulfill({
          status: 200,
          contentType: "application/json",
          body: JSON.stringify(LINKED),
        });
      }
      if (path.endsWith("/connectors")) {
        return route.fulfill({
          status: 200,
          contentType: "application/json",
          body: JSON.stringify([WHATSAPP_ROW]),
        });
      }
      return route.fulfill({
        status: 200,
        contentType: "application/json",
        body: "[]",
      });
    });

    await openSettings(page);

    const whatsapp = page.locator(row);
    await expect(whatsapp.locator(".status-chip")).toHaveAttribute(
      "data-status",
      "connected",
    );
    await expect(whatsapp.locator(".whatsapp-disclosure")).toHaveAttribute(
      "data-unofficial",
      "true",
    );
    await expect(whatsapp.locator(".whatsapp-disclosure")).toContainText(
      "Unofficial WhatsApp link",
    );
    await expect(whatsapp.locator(".whatsapp-meta")).toContainText(
      "2 chats on the phone · 1 allowed",
    );
    await expect(whatsapp.locator(".whatsapp-meta")).toContainText(
      "last sync: 2026-11-19 18:04",
    );

    await awaitSplashGone(page);
    await page.screenshot({
      path: `captures/${testInfo.project.name}-settings-whatsapp.png`,
      fullPage: true,
    });
  });

  test("a dead bridge shows the reason and offers a retry", async ({ page }) => {
    await page.route("**/api/v1/**", async (route) => {
      const path = new URL(route.request().url()).pathname;
      if (path.endsWith("/me")) {
        return route.fulfill({
          status: 200,
          contentType: "application/json",
          body: JSON.stringify(PROFILE),
        });
      }
      if (path.endsWith("/whatsapp")) {
        return route.fulfill({
          status: 200,
          contentType: "application/json",
          body: JSON.stringify(FAILED),
        });
      }
      if (path.endsWith("/connectors")) {
        return route.fulfill({
          status: 200,
          contentType: "application/json",
          body: JSON.stringify([WHATSAPP_ROW]),
        });
      }
      return route.fulfill({
        status: 200,
        contentType: "application/json",
        body: "[]",
      });
    });

    await openSettings(page);

    const whatsapp = page.locator(row);
    await expect(whatsapp.locator(".status-chip")).toHaveAttribute(
      "data-status",
      "error",
    );
    await expect(whatsapp.locator(".status-chip")).toHaveText("Bridge down");
    await expect(whatsapp).toContainText("the phone unlinked this device");
    await expect(whatsapp.locator(".whatsapp-action").first()).toBeVisible();
  });
});
