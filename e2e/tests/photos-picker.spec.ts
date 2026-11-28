import type { Page, Route } from "@playwright/test";
import { expect, test } from "@playwright/test";

import { awaitSplashGone } from "../support/splash";

// B5 acceptance, driven through the UI with `/api/v1/**` stubbed at the network
// boundary the way `settings.spec.ts` and `import-wizard.spec.ts` do (the visual
// job serves the release bundle statically, with no server behind it):
//
//   items listed     the pick's photos arrive as one row per photo — the note it
//                    became, its caption, and the tag it was filed under, each
//                    opening that tag; a receipt names the counts
//   import dedupes   a repeat pick lists the same photos marked already here,
//                    adds no second row, and writes nothing
//   503 unreachable  no UI path reaches `…/connectors/photos/preview` or
//                    `…/connectors/photos/import`, whose only answer for this
//                    connector is 503 — including a direct visit to the wizard
//                    route, which shows the pick instead of asking for a preview.
//                    The stub records every such hit and the tests assert none.
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

/** The two photos a pick turns into — distinct notes, distinct tags. */
const ITEMS = [
  {
    note_id: "6b2d4f80-0000-4000-8000-000000000301",
    title: "Sunrise over the bay",
    tag_id: TRAVEL.id,
    created: true,
  },
  {
    note_id: "6b2d4f80-0000-4000-8000-000000000302",
    title: "Breakfast at the market",
    tag_id: FOOD.id,
    created: true,
  },
];

/** Still open in Google's picker: keep waiting. */
const PENDING = {
  state: "pending",
  session: null,
  notes_created: 0,
  notes_skipped: 0,
  tags: [],
  items: [],
};

/** Two photos landed, filed under two tags — one row per photo. */
const IMPORTED = {
  state: "imported",
  session: null,
  notes_created: 2,
  notes_skipped: 0,
  tags: [TRAVEL, FOOD],
  items: ITEMS,
};

/** The same two photos picked again: nothing written, both skipped, both listed. */
const REPEAT = {
  state: "imported",
  session: null,
  notes_created: 0,
  notes_skipped: 2,
  tags: [TRAVEL, FOOD],
  items: ITEMS.map((item) => ({ ...item, created: false })),
};

/** The server's own refusal, quoted from `crates/server/src/connectors/photos.rs`. */
const PICK_ONLY_503 = {
  preview: "Google Photos only imports through a pick; there is nothing to preview",
  import: "Google Photos only imports through a pick; there is nothing to sync",
};

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
    if (path.endsWith("/preview")) {
      hits.preview.push(path);
      return fulfill(route, 503, { error: PICK_ONLY_503.preview });
    }
    if (path.endsWith("/import")) {
      hits.import.push(path);
      return fulfill(route, 503, { error: PICK_ONLY_503.import });
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

/** No UI path may reach the pair whose only answer for photos is 503. */
function assertNoDeadEnds(stub: { hits: { preview: string[]; import: string[] } }) {
  expect(stub.hits.preview, "no preview call may reach the 503").toEqual([]);
  expect(stub.hits.import, "no import call may reach the 503").toEqual([]);
}

test.describe("photos picker at 390px", () => {
  test.use({ viewport: { width: 390, height: 844 } });

  test("starts a pick, lists every photo, and reports the receipt", async ({
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
    await expect(row.locator('a[href$="/import"]')).toHaveCount(0);

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

    // Items listed: one row per picked photo — its own note, caption and tag.
    const items = row.locator('[data-testid="photos-picked-item"]');
    await expect(items).toHaveCount(ITEMS.length);
    for (const [index, item] of ITEMS.entries()) {
      const line = items.nth(index);
      await expect(line).toHaveAttribute("data-note-id", item.note_id);
      await expect(line).toHaveAttribute("data-item-created", "true");
      await expect(line).toContainText(item.title);
      const tag = line.locator('[data-testid="photos-picked-item-tag"]');
      await expect(tag).toHaveAttribute("href", `/tags/${item.tag_id}`);
    }
    await expect(items.nth(0)).toContainText("Travel");
    await expect(items.nth(1)).toContainText("Food");
    // A first pick is all new: nothing reads as already here.
    await expect(
      row.locator('[data-testid="photos-picked-item"]').filter({ hasText: "already here" }),
    ).toHaveCount(0);

    expect(stub.polls(), "the open pick is polled until it lands").toBeGreaterThan(0);
    assertNoDeadEnds(stub);

    // 390px has to fit what the row just grew.
    const overflows = await page.evaluate(
      () => document.documentElement.scrollWidth > window.innerWidth + 1,
    );
    expect(overflows, "the row must not scroll sideways at 390px").toBe(false);

    await awaitSplashGone(page);
  });

  test("a repeat pick writes nothing, lists the same photos as already here", async ({
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
    const items = row.locator('[data-testid="photos-picked-item"]');

    await row.locator('[data-testid="photos-start-pick"]').click();
    await expect(receipt).toHaveAttribute("data-pick-state", "imported", {
      timeout: 15_000,
    });
    await expect(items).toHaveCount(2);

    // Same photos, picked again: the server skips by media item id, and the row
    // must read as "nothing new" — not grow a second set of rows, and not lose
    // the ones it already showed.
    await row.locator('[data-testid="photos-start-pick"]').click();
    await expect(receipt).toContainText("Nothing new: all 2 picked photos", {
      timeout: 15_000,
    });
    await expect(receipt).toHaveAttribute("data-pick-state", "imported");
    await expect(items).toHaveCount(2);
    for (const [index, item] of ITEMS.entries()) {
      const line = items.nth(index);
      // The deduped photo is still listed, by identity, as the note it matched.
      await expect(line).toHaveAttribute("data-note-id", item.note_id);
      await expect(line).toHaveAttribute("data-item-created", "false");
      await expect(line).toContainText(item.title);
      await expect(line).toContainText("already here");
    }
    expect(stub.sessions(), "two picks, two sessions").toBe(2);
    assertNoDeadEnds(stub);
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
    assertNoDeadEnds(stub);
  });

  test("a direct visit to the import wizard shows the pick, never the 503", async ({
    page,
  }) => {
    const stub = await stubApi(page, { connectors: [PHOTOS] });

    // The wizard route still exists for every other connector; for photos it is
    // the one path that could reach `/preview` and `/import` unguarded.
    await page.goto("/connectors/photos/import");

    const notice = page.locator('[data-testid="photos-import-pick-only"]');
    await expect(notice).toBeVisible();
    await expect(notice).toContainText("Google Photos imports by picking");
    assertNoDeadEnds(stub);

    // And it hands over to the pick that does work.
    await notice.locator('[data-testid="photos-import-start-pick"]').click();
    await expect(page).toHaveURL(/\/connectors\/photos$/);
    await expect(
      page.locator('[data-connector-id="photos"] [data-testid="photos-start-pick"]'),
    ).toBeVisible();
    assertNoDeadEnds(stub);
  });
});

test.describe("photos picker on desktop", () => {
  // The mobile project already covers the 390px row; the desktop run is the one
  // with room for the connector screen and the wizard notice.
  const desktopOnly = () =>
    test.skip(
      test.info().project.name !== "desktop-1440",
      "the desktop project covers the wide layouts",
    );

  test("the settings row picks and lists every photo", async ({ page }) => {
    desktopOnly();
    let polled = 0;
    const stub = await stubApi(page, {
      connectors: [PHOTOS],
      poll: () => (polled++ === 0 ? PENDING : IMPORTED),
    });

    await openSettings(page);
    const row = page.locator('[data-connector-id="photos"]');

    await row.locator('[data-testid="photos-start-pick"]').click();
    await expect(row.locator('[data-testid="photos-picker-link"]')).toHaveAttribute(
      "href",
      SESSION.picker_uri,
    );

    const items = row.locator('[data-testid="photos-picked-item"]');
    await expect(items).toHaveCount(ITEMS.length, { timeout: 15_000 });
    await expect(items.nth(0)).toContainText(ITEMS[0].title);
    await expect(
      items.nth(0).locator('[data-testid="photos-picked-item-tag"]'),
    ).toHaveAttribute("href", `/tags/${TRAVEL.id}`);
    assertNoDeadEnds(stub);
  });

  test("replaces the import link with the pick panel on the connector screen", async ({
    page,
  }) => {
    desktopOnly();
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
    ).toHaveCount(ITEMS.length);
    assertNoDeadEnds(stub);
  });

  test("the wizard route shows the pick notice, not a preview", async ({ page }) => {
    desktopOnly();
    const stub = await stubApi(page, { connectors: [PHOTOS] });

    await page.goto("/connectors/photos/import");
    await expect(page.locator('[data-testid="photos-import-pick-only"]')).toBeVisible();
    assertNoDeadEnds(stub);
  });
});
