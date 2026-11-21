import { expect, test, type Page } from "@playwright/test";

import { PROFILE } from "../support/session";

import { awaitSplashGone } from "../support/splash";

// T31 — the `+` and the one sheet behind it. The API is stubbed at the network boundary, so
// these read the render path (open → type → save → grid) and the *order* of the writes, which
// is the whole claim: a new tag is filed before the note, and a note that fails afterwards
// leaves the tag made and says so.
const CATEGORY_ID = "3f0c1f7a-0000-4000-8000-0000000000c1";
const KYOTO_ID = "3f0c1f7a-0000-4000-8000-0000000000b1";
const KYOTO_THREAD_ID = "3f0c1f7a-0000-4000-8000-0000000000f1";
const OSLO_ID = "3f0c1f7a-0000-4000-8000-0000000000b9";
const NEW_THREAD_ID = "3f0c1f7a-0000-4000-8000-0000000000f9";
const NOW = "2026-11-20T09:00:00Z";

const KYOTO = {
  id: KYOTO_ID,
  name: "Kyoto",
  category_id: CATEGORY_ID,
  thread_count: 1,
  created_at: NOW,
};

const KYOTO_THREAD = {
  id: KYOTO_THREAD_ID,
  tag_id: KYOTO_ID,
  title: "The bamboo grove before the buses",
  source_count: 1,
  updated_at: NOW,
};

/** The title the server derives from a body: its first line, whitespace collapsed. */
function firstLine(body: string): string {
  return body.split("\n")[0].trim();
}

type Stub = { writes: string[]; bodies: { path: string; body: unknown }[] };

/**
 * Stand the server in behind the bundle.
 *
 * `threadStatus` is the status each `POST /threads` answers, in order; the last entry repeats,
 * so `[500]` is "the note write always fails" and `[500, 200]` is "it fails once".
 */
async function serve(page: Page, threadStatus: number[] = []): Promise<Stub> {
  const stub: Stub = { writes: [], bodies: [] };
  let attempt = 0;

  await page.route("**/api/v1/**", async (route) => {
    const request = route.request();
    const path = new URL(request.url()).pathname;
    const method = request.method();
    const json = (body: unknown) =>
      route.fulfill({
        status: 200,
        contentType: "application/json",
        body: JSON.stringify(body),
      });

    if (path.endsWith("/me")) {
      return json(PROFILE);
    }
    if (path.endsWith("/categories")) {
      return json([
        { id: CATEGORY_ID, name: "Travel", color: "#6aa9ff", position: 0 },
      ]);
    }
    if (/\/tags\/[^/]+\/threads$/.test(path) && method === "GET") {
      return json([KYOTO_THREAD]);
    }
    if (path.endsWith("/tags") && method === "GET") {
      return json([KYOTO]);
    }
    if (path.endsWith("/tags") && method === "POST") {
      stub.writes.push("POST /tags");
      const sent = request.postDataJSON() as { name: string; category_id: string };
      stub.bodies.push({ path, body: sent });
      return json({
        id: OSLO_ID,
        name: sent.name,
        category_id: sent.category_id,
        thread_count: 0,
        created_at: NOW,
      });
    }
    if (path.endsWith("/threads") && method === "POST") {
      stub.writes.push("POST /threads");
      const sent = request.postDataJSON() as { body: string; tag_id: string };
      stub.bodies.push({ path, body: sent });
      const status =
        threadStatus[Math.min(attempt++, Math.max(threadStatus.length - 1, 0))];
      if (status && status !== 200) {
        return route.fulfill({
          status,
          contentType: "application/json",
          body: JSON.stringify({ error: "the note store is down" }),
        });
      }
      return json({
        id: NEW_THREAD_ID,
        tag_id: sent.tag_id,
        title: firstLine(sent.body),
        source_count: 1,
        updated_at: NOW,
      });
    }
    return route.fulfill({ status: 404, body: "unexpected request" });
  });

  return stub;
}

/** Open the grid with the stub in place and the splash out of the way. */
async function openGrid(page: Page): Promise<void> {
  await page.goto("/");
  await awaitSplashGone(page);
  await expect(page.locator("#grid [data-tag-id]")).toHaveCount(1);
}

/** The `+`, opened the way a finger would. */
async function openSheet(page: Page): Promise<void> {
  await page.locator('[data-testid="new-strand-fab"]').click();
  await expect(page.locator("#new-strand-sheet")).toBeVisible();
}

/** The tag combobox is a controlled field: a synthetic one-shot fill does not reach Dioxus. */
async function typeTag(page: Page, name: string): Promise<void> {
  await page.locator("#new-strand-tag").pressSequentially(name, { delay: 20 });
}

test.describe("new strand", () => {
  test("a note filed under an existing tag becomes that card's newest row", async ({
    page,
  }, testInfo) => {
    const stub = await serve(page);
    await openGrid(page);
    await openSheet(page);

    // The sheet takes focus on the tag field: the keyboard path starts here.
    await expect(page.locator("#new-strand-tag")).toBeFocused();

    await typeTag(page, "Kyo");
    await page.locator('[data-testid="new-strand-option"][data-name="Kyoto"]').click();
    // An exact match offers no "Create" row: the tag is a choice, not a new name.
    await expect(page.locator("#new-strand-create")).toHaveCount(0);

    const title = "Paper lanterns on Pontocho";
    const note = "Every shop closes at the same hour and the street goes quiet in one pass.";
    await page.locator("#new-strand-title").fill(title);
    await page.locator("#new-strand-note").fill(note);

    // The counter is the body's budget, and it is live.
    const left = 5000 - (title.length + 2 + note.length);
    await expect(page.locator("#new-strand-count")).toHaveText(`${left} left`);

    // A double tap is still one write: the first click flips the guard before the second lands.
    const save = page.locator('[data-testid="new-strand-save"]');
    await save.evaluate((element) => {
      (element as HTMLElement).click();
      (element as HTMLElement).click();
    });

    await expect(page.locator("#new-strand-sheet")).toHaveCount(0);
    // Closing hands the focus back to the control that opened the sheet.
    await expect(page.locator('[data-testid="new-strand-fab"]')).toBeFocused();
    expect(stub.writes, "an existing tag is never created again").toEqual([
      "POST /threads",
    ]);
    expect(stub.bodies[0].body).toMatchObject({ tag_id: KYOTO_ID });

    // The new row is in the Kyoto card without a reload, first in the peek list.
    const card = page.locator(`#grid [data-tag-id="${KYOTO_ID}"]`);
    await expect(card).toContainText(title);
    await expect(card.locator(".peek-title").first()).toHaveText(title);
    // The grid still holds exactly the one card the stub served.
    await expect(page.locator("#grid [data-tag-id]")).toHaveCount(1);

    // Announced, not just drawn: the polite status line carries the result.
    await expect(page.locator('[data-testid="new-strand-announce"]')).toContainText(
      title,
    );

    if (testInfo.project.name === "mobile-360") {
      await page.screenshot({ path: "../docs/media/ui/new-strand-created.png" });
    }
  });

  test("a new tag is created first, then the note is filed under it", async ({
    page,
  }) => {
    const stub = await serve(page);
    await openGrid(page);
    await openSheet(page);

    await expect(page.locator("#new-strand-count")).toHaveText("5000 left");

    await typeTag(page, "Oslo");
    // No exact match, so the sheet offers to make it — and asks which category it belongs to.
    const create = page.locator("#new-strand-create");
    await expect(create).toContainText('Create “Oslo”');
    await page.locator('#new-strand-sheet [data-cat="travel"]').click();

    const title = "Ferry timetables, not the museum";
    await page.locator("#new-strand-title").fill(title);
    await page.locator("#new-strand-note").fill("The 07:20 to Bygdøy is the one that is empty.");
    await page.locator('[data-testid="new-strand-save"]').click();

    await expect(page.locator("#new-strand-sheet")).toHaveCount(0);
    // The order is the contract: the tag exists before the thread that points at it.
    expect(stub.writes).toEqual(["POST /tags", "POST /threads"]);
    expect(stub.bodies[0].body).toMatchObject({
      name: "Oslo",
      category_id: CATEGORY_ID,
    });
    expect(stub.bodies[1].body).toMatchObject({ tag_id: OSLO_ID });

    // A card that did not exist before is now in the grid, with the note inside it.
    const card = page.locator(`#grid [data-tag-id="${OSLO_ID}"]`);
    await expect(card).toBeVisible();
    await expect(card.locator(".name")).toHaveText("Oslo");
    await expect(card).toContainText(title);
    await expect(page.locator("#grid [data-tag-id]")).toHaveCount(2);
    await expect(page.locator("#feed-count")).toContainText("2 tags");
  });

  test("a note that fails after the tag was made says so, and the retry saves the note only", async ({
    page,
  }) => {
    // The tag write answers, the note write does not: exactly the half-done state.
    const stub = await serve(page, [500, 200]);
    await openGrid(page);
    await openSheet(page);

    await typeTag(page, "Oslo");
    await page.locator('#new-strand-sheet [data-cat="travel"]').click();
    await page.locator("#new-strand-title").fill("Ferry timetables, not the museum");
    await page.locator('[data-testid="new-strand-save"]').click();

    const error = page.locator("#new-strand-error");
    await expect(error).toBeVisible();
    await expect(error).toHaveAttribute("role", "alert");
    // It says which half landed, by name, and that the retry is the note's alone.
    await expect(error).toContainText("“Oslo” was created");
    await expect(error).toContainText("Retry saves the note only");
    await expect(error).toContainText("server returned 500");

    // The sheet stays up with the tag locked and visible: the next write cannot fork a second
    // tag, and the user can see what already exists.
    await expect(page.locator("#new-strand-sheet")).toBeVisible();
    await expect(page.locator("#new-strand-filed")).toContainText("Oslo");
    await expect(page.locator("#new-strand-tag")).toHaveCount(0);
    expect(stub.writes).toEqual(["POST /tags", "POST /threads"]);

    await page.locator("#new-strand-retry").click();

    await expect(page.locator("#new-strand-sheet")).toHaveCount(0);
    expect(stub.writes, "the retry is the note's write, not a second tag").toEqual([
      "POST /tags",
      "POST /threads",
      "POST /threads",
    ]);
    expect(stub.bodies[2].body).toMatchObject({ tag_id: OSLO_ID });

    const card = page.locator(`#grid [data-tag-id="${OSLO_ID}"]`);
    await expect(card).toBeVisible();
    await expect(card).toContainText("Ferry timetables, not the museum");
  });

  test("the sheet opens, closes and gives focus back from the keyboard alone", async ({
    page,
  }, testInfo) => {
    await serve(page);
    await openGrid(page);

    const fab = page.locator('[data-testid="new-strand-fab"]');
    await expect(fab).toHaveAttribute("aria-label", "New strand");
    await expect(fab).toHaveAttribute("aria-haspopup", "dialog");

    await fab.focus();
    await page.keyboard.press("Enter");
    await expect(page.locator("#new-strand-sheet")).toBeVisible();
    await expect(fab).toHaveAttribute("aria-expanded", "true");

    // Escape closes it, and the `+` gets the focus back.
    await page.keyboard.press("Escape");
    await expect(page.locator("#new-strand-sheet")).toHaveCount(0);
    await expect(fab).toBeFocused();
    await expect(fab).toHaveAttribute("aria-expanded", "false");

    // Same from the backdrop: a tap outside the panel is a dismissal.
    await fab.click();
    await expect(page.locator("#new-strand-sheet")).toBeVisible();
    await page.mouse.click(4, 4);
    await expect(page.locator("#new-strand-sheet")).toHaveCount(0);
    await expect(fab).toBeFocused();

    if (testInfo.project.name === "mobile-360") {
      // The sheet's own capture: open, filled, with the tag list showing.
      await fab.click();
      await typeTag(page, "Kyo");
      await page.locator('[data-testid="new-strand-option"][data-name="Kyoto"]').click();
      await page.locator("#new-strand-title").fill("Paper lanterns on Pontocho");
      await page
        .locator("#new-strand-note")
        .fill("Every shop closes at the same hour and the street goes quiet in one pass.");
      await awaitSplashGone(page);
      await page.screenshot({ path: "../docs/media/ui/new-strand.png" });
    }
  });
});
