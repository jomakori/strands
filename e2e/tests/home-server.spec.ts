import { expect, test } from "@playwright/test";

import { SECOND_SESSION_ID, THIRD_SESSION_ID, signedInAs } from "../support/session";

// Ticket T25. The grid is a read of the server, not of a fixture compiled into the Wasm: these
// specs drive the shipped bundle against `serve.py`, which seeds two accounts with disjoint rows
// and keeps whatever the browser writes in memory. So each claim here is about a request that
// crossed the network:
//
//   1. the signed-in account's rows are the ones on screen — and only those;
//   2. a reload lands on the same rows (they came from the server, not from a signal that
//      happens to still be alive);
//   3. the header counts what the server counted;
//   4. a captured note is the server's row: it appears in the grid and is still there after a
//      reload, which a fixture-backed grid could not do;
//   5. the reads have states: a skeleton while they are in flight, an empty grid when the
//      answer is empty, and a retry that re-issues the call when the answer is a 5xx.
//
// The suite default is the first seeded account (`signedIn` in playwright.config.ts); the
// isolation case opens its own context as the second one.
test.describe("home grid reads the server", () => {
  test("renders the signed-in account's tags, and only those", async ({ page }) => {
    await page.goto("/");

    const tags = page.locator("#grid [data-tag-id]");
    // The fixture's count would be the same, so what proves the source is the identity of the
    // rows, not the number. (Exact counts are asserted on the second account below: this is the
    // default account, and the writing specs add rows to it.)
    await expect(tags.first()).toBeVisible();
    await expect(page.locator("#grid")).toContainText("Tokyo");

    // Nothing from the other seeded account leaks into this one's grid.
    await expect(page.locator("#grid")).not.toContainText("Bouldering");
    await expect(page.locator("#grid")).not.toContainText("Night trains");
  });

  test("an empty answer is an empty grid, not a sample one", async ({ browser }) => {
    // The empty case is driven by holding the tag read open until it answers nothing: the app
    // must render the first-run notice, which a compiled-in sample grid made impossible. The
    // copy is T29's import-first one — zero tags anywhere is the new-account state, so the way
    // out is a connector, not a blank capture form (first-run.spec.ts owns that wording).
    const context = await browser.newContext({ storageState: signedInAs(SECOND_SESSION_ID) });
    const page = await context.newPage();
    await page.route("**/api/v1/tags", (route) => route.fulfill({ json: [] }));
    await page.goto("/");

    await expect(page.locator("#grid [data-tag-id]")).toHaveCount(0);
    await expect(page.locator('.empty-notice[data-state="empty"]')).toContainText(
      "No strands yet",
    );
    // The counts are the server's: it listed no rows, so there is nothing to count.
    await expect(page.locator("#feed-count")).toHaveText("0 tags · 0 threads");
    await context.close();
  });

  test("the grid stays on a skeleton until the read answers", async ({ page }) => {
    let release: (() => void) | undefined;
    const held = new Promise<void>((resolve) => {
      release = resolve;
    });
    await page.route("**/api/v1/tags", async (route) => {
      await held;
      await route.continue();
    });

    await page.goto("/");

    // The skeleton is the loading state: no rows yet, and a status label saying what is being
    // read, so the wait is announced rather than blank.
    const skeleton = page.locator('.skeleton[data-state="loading"]');
    await expect(skeleton).toBeVisible();
    await expect(skeleton).toContainText("Reading your tags");
    await expect(skeleton.locator(".skeleton-line")).toHaveCount(6);
    await expect(page.locator("#grid [data-tag-id]")).toHaveCount(0);
    await expect(page.locator("#feed-count")).toHaveText("");

    release?.();
    await expect(page.locator("#grid [data-tag-id]").first()).toBeVisible();
    await expect(skeleton).toHaveCount(0);
  });

  test("a 5xx on the read offers a retry that re-issues it", async ({ page }) => {
    let answered = 0;
    const calls: string[] = [];
    await page.route("**/api/v1/tags", (route) => {
      calls.push("GET /api/v1/tags");
      answered += 1;
      if (answered === 1) {
        return route.fulfill({ status: 503, contentType: "text/plain", body: "unavailable" });
      }
      return route.continue();
    });

    await page.goto("/");

    const notice = page.locator(".load-error").first();
    await expect(notice).toBeVisible();
    await expect(notice).toContainText(/unavailable|try again/i);
    await expect(page.locator("#grid [data-tag-id]")).toHaveCount(0);

    const before = calls.length;
    await notice.locator('button[data-retry="true"]').click();
    await expect.poll(() => calls.length).toBeGreaterThan(before);
    await expect(page.locator("#grid [data-tag-id]").first()).toBeVisible();
    await expect(page.locator(".load-error")).toHaveCount(0);
  });

  test("the second account sees its own rows, not the first's", async ({ browser }) => {
    const context = await browser.newContext({ storageState: signedInAs(SECOND_SESSION_ID) });
    const page = await context.newPage();
    await page.goto("/");

    await expect(page.locator("#grid [data-tag-id]")).toHaveCount(3);
    await expect(page.locator("#grid")).toContainText("Bouldering");
    await expect(page.locator("#grid")).not.toContainText("Tokyo");

    // Same account, second load: the rows come back because the server still has them.
    await page.reload();
    await expect(page.locator("#grid [data-tag-id]")).toHaveCount(3);
    await expect(page.locator("#grid")).toContainText("Bouldering");
    await context.close();
  });

  test("the header counts the server's rows", async ({ browser }) => {
    // The second account, because no other spec writes to it: three tags, one thread apiece.
    const context = await browser.newContext({ storageState: signedInAs(SECOND_SESSION_ID) });
    const page = await context.newPage();
    await page.goto("/");

    await expect(page.locator("#feed-count")).toHaveText("3 tags · 3 threads");
    await expect(page.locator("#grid [data-tag-id]")).toHaveCount(3);
    await context.close();
  });

  test("a captured note is a server row, and survives a reload", async ({ browser }) => {
    // The writing case gets its own account: what it creates must not skew another spec's count.
    // Everything it writes is named per run, because the seed server keeps state between runs.
    const context = await browser.newContext({ storageState: signedInAs(THIRD_SESSION_ID) });
    const page = await context.newPage();
    await page.goto("/capture");

    const stamp = Date.now().toString(36).slice(-5);
    const tagName = `Porto ${stamp}`;
    const note = `Tile patterns on the riverside wall ${stamp}`;
    await page.locator('[data-testid="capture-body"]').fill(note);
    // A tag the server does not hold yet: the type-ahead offers to create it, and the category
    // chip names where it goes. Typed through the keyboard, not filled: the box is a controlled
    // input, and a value set straight onto the element never reaches Dioxus's handler.
    const tagField = page.locator('[data-testid="capture-tag"]');
    await tagField.click();
    await page.keyboard.type(tagName);
    await expect(page.locator('[data-testid="capture-create"]')).toBeVisible();
    await page.locator('[data-cat="travel"]').click();
    await page.locator('[data-testid="capture-save"]').click();

    // Landed back on the grid, inside a card the server created for it.
    await expect(page).toHaveURL(/\/$/);
    const created = page.locator("#grid [data-tag-id]").filter({ hasText: tagName });
    await expect(created).toHaveCount(1);
    await expect(created).toContainText(note);

    // The reload asks the server again; the fixture this replaced could not have answered it
    // with a row it never held, so this is the claim that the source really moved.
    await page.reload();
    const afterReload = page.locator("#grid [data-tag-id]").filter({ hasText: tagName });
    await expect(afterReload).toHaveCount(1);
    await expect(afterReload).toContainText(note);
    await context.close();
  });
});
