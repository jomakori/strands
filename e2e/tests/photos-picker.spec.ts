import type { Page, Route } from "@playwright/test";
import { expect, test } from "@playwright/test";

import { awaitSplashGone } from "../support/splash";

// B5 acceptance, driven through the UI with `/api/v1/**` stubbed at the network
// boundary the way `settings.spec.ts` and `import-wizard.spec.ts` do (the visual
// job serves the release bundle statically, with no server behind it):
//
//   items listed     the pick's tags arrive as rows, each opening its own tag;
//                    a receipt names the counts the server reported
//   import dedupes   a repeat pick reports its skips and adds no second row
//   503 unreachable  no control leads to `…/connectors/photos/preview` or
//                    `/import`, whose only answer for this connector is 503 —
//                    the stub records every such hit and the tests assert none
//
// The picker itself (Google's window, the download URLs) cannot be exercised
// here; `crates/server/tests/photos_picker.rs` owns that against a real
// Postgres. What this file owns is that the browser never takes the dead route
// and that the panel renders each state the poll can report.
const PROFILE = {
  id: "9f3d2a1e-0000-4000-8000-00000000a001",
  email: "alice@example.com",
  name: "Alice",
  provider: "google",
};

/** The registry id `connector-photos` registers under — not a display name. */
const PHOTOS = {
  id: "photos",
  label: "Google Photos",
  scopes: ["https://www.googleapis.com/auth/photometadata"],
  status: "connected",
  error: null,
};

const DISCONNECTED_PHOTOS = { ...PHOTOS, status: "disconnected" };

const GMAIL = {
  id: "gmail",
  label: "Gmail",
  scopes: ["gmail.readonly"],
  status: "needs_sign_in",
  error: null,
};

const SESSION = {
  session_id: "pick-1",
  picker_uri: "https://photos.google.com/picker/session/pick-1",
  expires_at: "2026-11-28T06:00:00Z",
};

const TRAVEL = {
  id: "3f1c1d6a-0000-4000-8000-000000000101",
  name: "Travel",
  category_id: "3f1c1d6a-0000-4000-8000-000000000201",
  thread_count: 2,
  created_at: "2026-11-28T04:00:00Z",
};

const FOOD = {
  id: "3f1c1d6a-0000-4000-8000-000000000102",
  name: "Food",
  category_id: "3f1c1d6a-0000-4000-8000-000000000201",
  thread_count: 1,
  created_at: "2026-11-28T04:00:01Z",
};

/** Still open in Google's picker: keep waiting. */
const PENDING = { state: "pending", session: null, notes_created: 0, notes_skipped: 0, tags: [] };

/** Two photos landed, filed under two tags. */
const IMPORTED = {
  state: "imported",
  session: null,
  notes_created: 2,
  notes_skipped: 0,
  tags: [TRAVEL, FOOD],
};

/** The same two photos picked again: nothing written, both skipped. */
const REPEAT = {
  state: "imported",
  session: null,
  notes_created: 0,
  notes_skipped: 2,
  tags: [TRAVEL, FOOD],
};

/** The exact body `crates/server/src/connectors/photos.rs` answers these with. */
const PICK_ONLY_503 =
  "Google Photos only imports through a pick; open Settings → Google Photos and pick there.";

type Stubs = {
  connectors?: unknown[];
  /** What the next `POST …/photos/sessions` answers. Defaults to `SESSION`. */
  session?: () => unknown;
  /** What the next poll of an open session answers. Defaults to `PENDING`. */
  poll?: () => unknown;
};

function fulfill(route: Route, status: number, body: unknown) {
  return route.fulfill({
    status,
    contentType: "application/json",
    body: JSON.stringify(body),
  });
}

/**
 * Stub the whole API surface, counting the calls that matter.
 *
 * The preview/import pair is stubbed at its real answer (503) rather than left
 * to fall through: a hit is recorded *and* answered, so a test that watches the
 * counter sees the dead end the UI must never take.
 */
async function stubApi(page: Page, stubs: Stubs = {}) {
  const hits = { preview: [] as string[], import: [] as string[] };
  let sessions = 0;
  let polls = 0;

  await page.route("**/api/v1/**", async (route) => {
    const path = new URL(route.request().url()).pathname;
    const method = route.request().method();

    if (path.endsWith("/me")) {
      return fulfill(route, 200, PROFILE);
    }
    if (path.endsWith("/connectors")) {
      return fulfill(route, 200, stubs.connectors ?? [PHOTOS]);
    }
    if (path.endsWith("/connectors/photos/sessions") && method === "POST") {
      sessions += 1;
      return fulfill(route, 201, stubs.session ? stubs.session() : SESSION);
    }
    if (path.includes("/connectors/photos/sessions/") && method === "GET") {
      polls += 1;
      return fulfill(route, 200, stubs.poll ? stubs.poll() : PENDING);
    }
    if (path.endsWith("/preview") || path.endsWith("/import")) {
      const bucket = path.endsWith("/preview") ? hits.preview : hits.import;
      bucket.push(path);
      return fulfill(route, 503, { error: PICK_ONLY_503 });
    }
    return fulfill(route, 200, []);
  });

  return {
    hits,
    sessions: () => sessions,
    polls: () => polls,
  };
}

/** Settings is reached the way a user reaches it: the `,` binding, after signing in. */
async function openSettings(page: Page) {
  await page.goto("/");
  await expect(page.locator("#account-link")).toBeVisible();
  await page.keyboard.press(",");
  await expect(page).toHaveURL(/\/settings$/);
}

test.describe("photos picker at 390px", () => {
  test.use({ viewport: { width: 390, height: 844 } });

  test("starts a pick, lists what arrived, and reports the receipt", async ({
    page,
  }) => {
    let polled = 0;
    const stub = await stubApi(page, {
      connectors: [PHOTOS, GMAIL],
      poll: () => (polled++ === 0 ? PENDING : IMPORTED),
    });

    await openSettings(page);

    const row = page.locator('[data-connector-id="photos"]');
    await expect(row.locator(".status-chip")).toHaveAttribute(
      "data-status",
      "connected",
    );
    // The generic import entry is gone from this connector's row: it is the
    // control whose only answer is the 503 below.
    await expect(row.locator('[data-testid="connector-import-link"]')).toHaveCount(0);

    await row.locator('[data-testid="photos-start-pick"]').click();

    // The session is open: the panel hands over Google's own picker window.
    const picker = row.locator('[data-testid="photos-picker-link"]');
    await expect(picker).toBeVisible();
    await expect(picker).toHaveAttribute("href", SESSION.picker_uri);
    await expect(picker).toHaveAttribute("target", "_blank");
    expect(stub.sessions()).toBe(1);

    const receipt = row.locator('[data-testid="photos-receipt"]');
    await expect(receipt).toHaveAttribute("data-pick-state", "imported", {
      timeout: 15_000,
    });
    await expect(receipt).toContainText("Imported 2 photos.");

    // Items listed: one row per tag the notes were filed under, each a link.
    const items = row.locator('[data-testid="photos-picked-item"]');
    await expect(items).toHaveCount(2);
    await expect(items.nth(0)).toContainText("Travel");
    await expect(items.nth(1)).toContainText("Food");
    await expect(items.nth(0).locator("a")).toHaveAttribute(
      "href",
      new RegExp(`/tags/${TRAVEL.id}$`),
    );

    expect(stub.polls(), "the open pick is polled until it lands").toBeGreaterThan(0);
    expect(stub.hits.preview, "no preview call from the photos row").toEqual([]);
    expect(stub.hits.import, "no import call from the photos row").toEqual([]);

    // 390px has to fit what the row just grew.
    const overflows = await page.evaluate(
      () => document.documentElement.scrollWidth > window.innerWidth + 1,
    );
    expect(overflows, "the row must not scroll sideways at 390px").toBe(false);

    await awaitSplashGone(page);
  });

  test("a repeat pick writes nothing and says what it skipped", async ({
    page,
  }) => {
    let polled = 0;
    const stub = await stubApi(page, {
      connectors: [PHOTOS],
      poll: () => (polled++ === 0 ? IMPORTED : REPEAT),
    });

    await openSettings(page);
    const row = page.locator('[data-connector-id="photos"]');
    const receipt = row.locator('[data-testid="photos-receipt"]');

    await row.locator('[data-testid="photos-start-pick"]').click();
    await expect(receipt).toHaveAttribute("data-pick-state", "imported", {
      timeout: 15_000,
    });
    await expect(row.locator('[data-testid="photos-picked-item"]')).toHaveCount(2);

    // Same photos, picked again: the server skips by media item id, and the row
    // must read as "nothing new" rather than growing a second set of items.
    await row.locator('[data-testid="photos-start-pick"]').click();
    await expect(receipt).toContainText("Nothing new: all 2 picked photos", {
      timeout: 15_000,
    });
    await expect(receipt).toHaveAttribute("data-pick-state", "imported");
    await expect(row.locator('[data-testid="photos-picked-item"]')).toHaveCount(2);
    expect(stub.sessions(), "two picks, two sessions").toBe(2);
    expect(stub.hits.preview).toEqual([]);
    expect(stub.hits.import).toEqual([]);
  });

  test("without a grant it offers the grant, not a pick", async ({ page }) => {
    const stub = await stubApi(page, { connectors: [DISCONNECTED_PHOTOS] });

    await openSettings(page);
    const row = page.locator('[data-connector-id="photos"]');

    await expect(row.locator(".status-chip")).toHaveAttribute(
      "data-status",
      "disconnected",
    );
    await expect(row.locator('[data-testid="photos-start-pick"]')).toHaveCount(0);
    await expect(row.locator('[data-testid="photos-connect"]')).toHaveAttribute(
      "href",
      "/api/v1/connectors/google_photos/connect",
    );
    expect(stub.sessions(), "a pick needs a grant; none was asked for").toBe(0);
    expect(stub.hits.preview).toEqual([]);
    expect(stub.hits.import).toEqual([]);
  });
});

test.describe("photos picker on the connector screen", () => {
  test("replaces the import link with the pick panel", async ({ page }) => {
    // The desktop run is the one with room for the detail screen's layout; the
    // mobile project already covers the row above.
    test.skip(
      test.info().project.name !== "desktop-1440",
      "the desktop project covers the connector screen",
    );
    let polled = 0;
    const stub = await stubApi(page, {
      connectors: [PHOTOS],
      poll: () => (polled++ === 0 ? PENDING : IMPORTED),
    });

    await openSettings(page);
    await page
      .locator('[data-connector-id="photos"] .connector-open')
      .click();
    await expect(page).toHaveURL(/\/connectors\/photos$/);

    const panel = page.locator('[data-connector-id="photos"]');
    await expect(panel.locator('[data-testid="connector-import-link"]')).toHaveCount(0);

    await panel.locator('[data-testid="photos-start-pick"]').click();
    await expect(
      panel.locator('[data-testid="photos-picker-link"]'),
    ).toHaveAttribute("href", SESSION.picker_uri);
    await expect(
      panel.locator('[data-testid="photos-receipt"]'),
    ).toHaveAttribute("data-pick-state", "imported", { timeout: 15_000 });
    await expect(
      panel.locator('[data-testid="photos-picked-item"]'),
    ).toHaveCount(2);

    expect(stub.hits.preview, "the screen must not reach the 503 preview").toEqual([]);
    expect(stub.hits.import, "the screen must not reach the 503 import").toEqual([]);
  });
});
