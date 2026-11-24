import type { Page } from "@playwright/test";
import { expect, test } from "@playwright/test";

import { awaitSplashGone } from "../support/splash";

const PROFILE = {
  id: "9f3d2a1e-0000-4000-8000-00000000b001",
  email: "alice@example.com",
  name: "Alice",
  avatar_url: null,
  provider: "google",
};

const MAPS_ROW = {
  id: "google-maps",
  label: "Google Maps",
  scopes: [],
  status: "connected",
  error: null,
};

const PREVIEW = {
  connector: "google-maps",
  items: [
    {
      item_key: "photo/Trips/harbour.jpg",
      label: "Harbour at dusk",
      timestamp: "2026-11-18T17:20:00Z",
      already_imported: false,
    },
    {
      item_key: "photo/Trips/beach.jpg",
      label: "Beach walk",
      timestamp: "2026-11-17T09:05:00Z",
      already_imported: false,
    },
    {
      item_key: "place/Trips/reykjavik",
      label: "Reykjavík",
      timestamp: "2026-11-16T12:00:00Z",
      already_imported: false,
    },
    {
      item_key: "link/Notes/strands.dev",
      label: "strands.dev",
      timestamp: "2026-11-15T08:00:00Z",
      already_imported: false,
    },
    {
      item_key: "voice/Trips/memo.m4a",
      label: "Voice memo",
      timestamp: "2026-11-14T19:45:00Z",
      already_imported: true,
    },
    {
      item_key: "text/Notes/weekly.md",
      label: "Weekly plan",
      timestamp: "2026-11-13T07:30:00Z",
      already_imported: false,
    },
  ],
};

const HISTORY = {
  connector: "google-maps",
  runs: [{ imported_at: "2026-11-20T18:04:00Z", imported: 3, skipped: 0 }],
};

// A freshly linked phone that has not pushed any history yet: no chats at all.
const WHATSAPP_LINKED_EMPTY = {
  state: { state: "linked" },
  disclosure: "Unofficial WhatsApp link.",
  allowlist: [],
  chats: [],
  last_sync_at: null,
  last_error: null,
};

// No session at all: never paired, or unlinked from the phone. The picker must
// not promise a sync that cannot arrive until the phone is linked again.
const WHATSAPP_UNLINKED_EMPTY = {
  state: { state: "unlinked" },
  disclosure: "Unofficial WhatsApp link.",
  allowlist: [],
  chats: [],
  last_sync_at: null,
  last_error: null,
};

// The bridge died on its own, with the server's reason to show verbatim.
const WHATSAPP_FAILED_EMPTY = {
  state: { state: "failed", reason: "the phone unlinked this device" },
  disclosure: "Unofficial WhatsApp link.",
  allowlist: [],
  chats: [],
  last_sync_at: null,
  last_error: "the phone unlinked this device",
};

function importResult(keys: string[]) {
  return {
    results: keys.map((key) => ({ item_key: key, outcome: "imported" })),
    imported: keys.length,
    skipped: 0,
    failed: 0,
  };
}

type Stub = {
  preview?: unknown;
  onImport?: (keys: string[]) => unknown;
  whatsapp?: unknown;
};

async function stubApi(page: Page, stub: Stub = {}) {
  await page.route("**/api/v1/**", async (route) => {
    const url = new URL(route.request().url());
    const path = url.pathname;

    if (path.endsWith("/me")) {
      return route.fulfill({
        status: 200,
        contentType: "application/json",
        body: JSON.stringify(PROFILE),
      });
    }
    if (path === "/api/v1/connectors") {
      return route.fulfill({
        status: 200,
        contentType: "application/json",
        body: JSON.stringify([MAPS_ROW]),
      });
    }
    if (path === "/api/v1/whatsapp") {
      return route.fulfill({
        status: 200,
        contentType: "application/json",
        body: JSON.stringify(stub.whatsapp ?? WHATSAPP_LINKED_EMPTY),
      });
    }
    if (path.endsWith("/preview")) {
      return route.fulfill({
        status: 200,
        contentType: "application/json",
        body: JSON.stringify(stub.preview ?? PREVIEW),
      });
    }
    if (path.endsWith("/import") && route.request().method() === "POST") {
      const body = JSON.parse(route.request().postData() ?? "{}");
      const keys: string[] = body.item_keys ?? [];
      return route.fulfill({
        status: 200,
        contentType: "application/json",
        body: JSON.stringify(stub.onImport?.(keys) ?? importResult(keys)),
      });
    }
    if (path.endsWith("/history")) {
      return route.fulfill({
        status: 200,
        contentType: "application/json",
        body: JSON.stringify(HISTORY),
      });
    }
    return route.fulfill({
      status: 200,
      contentType: "application/json",
      body: "[]",
    });
  });
}

async function openWizard(page: Page, connectorId = "google-maps") {
  await page.goto(`/connectors/${connectorId}/import`);
  await expect(page.locator("#import-title")).toHaveText(`Import from ${connectorId}`);
  await awaitSplashGone(page);
}

const card = '[data-testid="import-candidate"]';
const submit = '[data-testid="import-submit"]';
const summary = "#import-summary";

test.describe("import wizard", () => {
  test("the container step counts items and what is already imported", async ({
    page,
  }, testInfo) => {
    await stubApi(page);
    await openWizard(page);

    const trips = page.locator('[data-container="Trips"]');
    await expect(trips.locator(".container-count")).toHaveText(
      "4 items · 1 already imported",
    );

    const notes = page.locator('[data-container="Notes"]');
    await expect(notes.locator(".container-count")).toHaveText(
      "2 items · 0 already imported",
    );

    await expect(
      page.locator('[data-testid="import-container-all"] .container-count'),
    ).toHaveText("6 items");

    await page.screenshot({
      path: `captures/${testInfo.project.name}-import-containers.png`,
      fullPage: true,
    });
  });

  test("a filter narrows the gallery, and imported items cannot be ticked", async ({
    page,
  }, testInfo) => {
    await stubApi(page);
    await openWizard(page);

    await page.locator('[data-testid="import-container-all"]').click();
    await expect(page.locator(card)).toHaveCount(6);

    await page.locator('[data-filter="photo"]').click();
    await expect(page.locator(card)).toHaveCount(2);
    await expect(page.locator(`${card}[data-kind="photo"]`)).toHaveCount(2);

    await page.locator('[data-filter="all"]').click();
    const skipped = page.locator(`${card}[data-skipped="true"]`);
    await expect(skipped).toHaveCount(1);
    await expect(skipped).toHaveAttribute("aria-disabled", "true");
    await expect(skipped.locator('[data-testid="import-skipped-badge"]')).toHaveText(
      "Already imported",
    );

    await skipped.click({ force: true });
    await expect(skipped).toHaveAttribute("aria-checked", "false");

    await expect(page.locator(summary)).toHaveText("Importing 0 of 6 strands");
    await expect(page.locator(submit)).toBeDisabled();

    await awaitSplashGone(page);
    await page.screenshot({
      path: `captures/${testInfo.project.name}-import-review.png`,
      fullPage: true,
    });
  });

  test("importing sends exactly the ticked keys and reports what happened", async ({
    page,
  }) => {
    let sent: string[] | null = null;
    await stubApi(page, {
      onImport: (keys) => {
        sent = keys;
        return importResult(keys);
      },
    });
    await openWizard(page);

    await page.locator('[data-testid="import-container-all"]').click();
    await page.locator(`${card}[data-key="photo/Trips/harbour.jpg"]`).click();
    await page.locator(`${card}[data-key="place/Trips/reykjavik"]`).click();

    await expect(page.locator(summary)).toHaveText("Importing 2 of 6 strands");
    await expect(page.locator(submit)).toBeEnabled();

    await page.locator(submit).click();

    await expect(page.locator('[data-testid="import-result"]')).toContainText(
      "Imported 2 · skipped 0 · failed 0.",
    );
    expect(sent?.sort()).toEqual([
      "photo/Trips/harbour.jpg",
      "place/Trips/reykjavik",
    ]);

    await page.locator('[data-testid="import-history-toggle"]').click();
    const runs = page.locator('[data-testid="import-history-list"] li');
    await expect(runs).toHaveCount(1);
    await expect(runs.first()).toContainText("2026-11-20 18:04 — 3 imported");
  });

  test("a card toggles from the keyboard", async ({ page }) => {
    await stubApi(page);
    await openWizard(page);

    await page.locator('[data-testid="import-container-all"]').click();
    const first = page.locator(`${card}[data-key="photo/Trips/harbour.jpg"]`);
    await first.focus();
    await page.keyboard.press("Enter");
    await expect(first).toHaveAttribute("aria-checked", "true");

    await page.keyboard.press(" ");
    await expect(first).toHaveAttribute("aria-checked", "false");
    await expect(page.locator(summary)).toHaveText("Importing 0 of 6 strands");
  });

  test("back returns to the container list", async ({ page }) => {
    await stubApi(page);
    await openWizard(page);

    await page.locator('[data-container="Trips"]').click();
    await expect(page.locator(card)).toHaveCount(4);

    await page.locator('[data-testid="import-back"]').click();
    await expect(page.locator('[data-testid="import-container-all"]')).toBeVisible();
  });

  test("a connector with nothing new says so instead of showing an empty gallery", async ({
    page,
  }) => {
    await stubApi(page, {
      preview: {
        connector: "google-maps",
        items: PREVIEW.items.map((item) => ({ ...item, already_imported: true })),
      },
    });
    await openWizard(page);

    await expect(page.locator("body")).toContainText(
      "Everything here is already imported.",
    );
    await expect(page.locator(card)).toHaveCount(0);
  });

  // A3, three ways: zero chats means different things depending on the link, and
  // each one has to say what is actually true instead of counting zeroes.
  test("a linked phone with no chats says the history has not synced yet", async ({
    page,
  }, testInfo) => {
    await stubApi(page, { whatsapp: WHATSAPP_LINKED_EMPTY });
    await openWizard(page, "whatsapp");

    const body = page.locator("body");
    await expect(
      page.locator('[data-testid="import-zero-chats"][data-whatsapp-gate="linked-empty"]'),
    ).toBeVisible();
    await expect(body).toContainText("WhatsApp history has not synced yet.");
    await expect(body).toContainText("Chats appear here as the sync arrives");
    await expect(body).not.toContainText("WhatsApp is not linked.");
    await expect(body).not.toContainText("Imported 0 notes from 0 chats");
    await expect(body).not.toContainText("Could not load candidates");

    await expect(page.locator(card)).toHaveCount(0);
    await expect(page.locator(summary)).toHaveText("Importing 0 of 0 strands");
    await expect(page.locator(submit)).toBeDisabled();
    await expect(page.locator('[data-testid="import-open-settings"]')).toHaveCount(0);

    await page.screenshot({
      path: `captures/${testInfo.project.name}-import-whatsapp-linked-empty.png`,
      fullPage: true,
    });
  });

  test("an unlinked phone says the link is missing, not that a sync is coming", async ({
    page,
  }, testInfo) => {
    await stubApi(page, { whatsapp: WHATSAPP_UNLINKED_EMPTY });
    await openWizard(page, "whatsapp");

    const body = page.locator("body");
    await expect(
      page.locator('[data-testid="import-zero-chats"][data-whatsapp-gate="unlinked"]'),
    ).toBeVisible();
    await expect(body).toContainText("WhatsApp is not linked.");
    await expect(body).toContainText("Link the phone from Settings");
    await expect(body).not.toContainText("history has not synced");
    await expect(body).not.toContainText("Imported 0 notes from 0 chats");
    await expect(body).not.toContainText("Could not load candidates");

    // With no link there is nothing an import could run on: the footer sends the
    // user to the screen that can fix it rather than offering a dead button.
    await expect(page.locator(card)).toHaveCount(0);
    await expect(page.locator(submit)).toHaveCount(0);
    await expect(page.locator(summary)).toHaveCount(0);
    await expect(page.locator('[data-testid="import-open-settings"]')).toBeVisible();

    await page.screenshot({
      path: `captures/${testInfo.project.name}-import-whatsapp-unlinked.png`,
      fullPage: true,
    });
  });

  test("a dead bridge shows the server's reason, not a sync promise", async ({
    page,
  }, testInfo) => {
    await stubApi(page, { whatsapp: WHATSAPP_FAILED_EMPTY });
    await openWizard(page, "whatsapp");

    const body = page.locator("body");
    await expect(
      page.locator('[data-testid="import-zero-chats"][data-whatsapp-gate="failed"]'),
    ).toBeVisible();
    await expect(body).toContainText("The WhatsApp bridge went down.");
    await expect(body).toContainText("the phone unlinked this device");
    await expect(body).not.toContainText("history has not synced");
    await expect(body).not.toContainText("Imported 0 notes from 0 chats");
    await expect(body).not.toContainText("Could not load candidates");

    await expect(page.locator(card)).toHaveCount(0);
    await expect(page.locator(submit)).toHaveCount(0);
    await expect(page.locator('[data-testid="import-open-settings"]')).toBeVisible();

    await page.screenshot({
      path: `captures/${testInfo.project.name}-import-whatsapp-failed.png`,
      fullPage: true,
    });
  });
});
