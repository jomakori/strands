import { expect, test, type Page } from "@playwright/test";

import { awaitSplashGone } from "../support/splash";

// Ticket T26 (spec §7.5). A phone has no `/`, so the sheet gets a second door: a drag downward
// on the strip at the head of the grid, which follows the finger and snaps open or back.
//
// The drag has to be real touch input, not `page.mouse`: the gesture ignores `pointerType:
// "mouse"` on purpose (a mouse drag across a list is a selection), so a synthetic mouse drag
// would prove nothing about the code that actually runs. `Input.dispatchTouchEvent` through
// CDP is what a compositor-driven pan looks like to the page, which is also what makes the
// `touch-action: none` on the strip matter — and what makes the scrolled case below a real
// test of the "only at offset 0" rule instead of a tautology.

/** The distance the gesture commits at. Mirrors `pull::OPEN_AT`; the assertion is the spec's. */
const OPEN_AT = 72;

/** Touch input straight through CDP: down, a run of moves, up. */
async function touchDrag(page: Page, from: { x: number; y: number }, dy: number, steps = 8) {
  const cdp = await page.context().newCDPSession(page);
  try {
    await cdp.send("Input.dispatchTouchEvent", {
      type: "touchStart",
      touchPoints: [{ x: from.x, y: from.y }],
    });
    for (let step = 1; step <= steps; step += 1) {
      // One move per step, so the page sees a drag a finger could have made rather than a
      // single jump the slop test could not tell from a tap.
      await cdp.send("Input.dispatchTouchEvent", {
        type: "touchMove",
        touchPoints: [{ x: from.x, y: from.y + (dy * step) / steps }],
      });
    }
    await cdp.send("Input.dispatchTouchEvent", { type: "touchEnd", touchPoints: [] });
  } finally {
    await cdp.detach();
  }
}

/** The middle of the pull strip, in viewport coordinates. */
async function stripCentre(page: Page) {
  const strip = page.locator("#pull");
  await expect(strip).toBeVisible();
  const box = await strip.boundingBox();
  if (!box) throw new Error("the pull strip has no box to start a drag on");
  return { x: box.x + box.width / 2, y: box.y + box.height / 2 };
}

/**
 * Waits for the sheet's panel to come to rest at the bottom edge of the viewport.
 *
 * The panel animates open, so its box moves for a couple of hundred milliseconds after the
 * sheet is already "visible". A drag started on a coordinate read mid-animation lands where
 * the grabber no longer is — the press has to wait for the sheet to arrive, exactly as a
 * finger would.
 */
async function settledPanel(page: Page) {
  const panel = page.locator("#search-sheet .sheet__panel");
  const viewport = page.viewportSize();
  if (!viewport) throw new Error("this project has no viewport to measure the panel against");
  await expect
    .poll(
      async () => {
        const box = await panel.boundingBox();
        return box ? Math.round(box.y + box.height) : -1;
      },
      { message: "the panel never settled at the bottom edge" },
    )
    .toBe(viewport.height);
  return panel;
}

test.describe("pull down to search", () => {
  test.skip(({ hasTouch }) => !hasTouch, "the gesture is a finger's, and this project has none");

  test("a pull from the head of the grid opens the sheet on the server's answer", async ({
    page,
  }, testInfo) => {
    // One note, one token: the row on screen has to be the row this spec answered with, or the
    // sheet could be painting a fixture and still pass.
    await page.route("**/api/v1/search*", (route) =>
      route.fulfill({
        json: [
          {
            id: "8d6b0f7e-1c2d-4a3b-9f10-2e4c6a8b0d13",
            title: "Kumquat tasting",
            snippet: "kumquat, candied peel, sea salt",
            tag_id: "6a1a3f6e-5b1b-4a2f-8d3e-6f9c1b2a4d55",
            tag_name: "food",
          },
        ],
      }),
    );

    await page.goto("/");
    await expect(page.locator("#grid [data-tag-id]").first()).toBeVisible();

    // Nothing pulled yet: the sheet is mounted by the shell but has to be out of the tree.
    const sheet = page.locator("#search-sheet");
    await expect(sheet).toHaveCount(0);

    await touchDrag(page, await stripCentre(page), OPEN_AT + 48);
    await expect(sheet).toBeVisible();

    // The sheet follows the finger and arrives, so it is where it rests and not somewhere
    // between: a panel left mid-slide would still be "visible" to the assertions above.
    const panel = await settledPanel(page);

    // A phone raises its keyboard only for a focused input, and the sheet exists to be typed in.
    const input = page.locator("#search-input");
    await expect(input).toBeFocused();

    await input.fill("kumquat");
    await input.press("Enter");
    await expect(sheet.locator(".sheet__results")).toContainText("Kumquat tasting");
    await expect(sheet.locator(".sheet__results")).toContainText("candied peel");

    await awaitSplashGone(page);
    await page.screenshot({ path: `captures/${testInfo.project.name}-pull-search.png` });
  });

  test("a pull short of the threshold leaves the sheet closed and the grid in place", async ({
    page,
  }) => {
    await page.goto("/");
    await expect(page.locator("#grid [data-tag-id]").first()).toBeVisible();

    const before = await page.evaluate(() => window.scrollY);
    await touchDrag(page, await stripCentre(page), OPEN_AT - 30);

    // The panel comes back rather than appearing: it is mounted for the slide home and then
    // gone, so the wait is for the count, not for a visibility toggle.
    await expect(page.locator("#search-sheet")).toHaveCount(0, { timeout: 2000 });
    expect(await page.evaluate(() => window.scrollY)).toBe(before);
  });

  test("a drag from a scrolled grid scrolls and never opens the sheet", async ({ page }) => {
    await page.goto("/");
    await expect(page.locator("#grid [data-tag-id]").first()).toBeVisible();

    // Enough rows to have somewhere to scroll to; the seeded account has a full grid.
    await page.evaluate(() => window.scrollTo(0, 400));
    await expect.poll(() => page.evaluate(() => window.scrollY)).toBeGreaterThan(300);

    const card = await page.locator("#grid [data-tag-id]").first().boundingBox();
    if (!card) throw new Error("no card to drag from");
    await touchDrag(page, { x: card.x + card.width / 2, y: card.y + card.height / 2 }, 160);

    // The page moved instead: a downward drag with the list already scrolled is the list's.
    await expect.poll(() => page.evaluate(() => window.scrollY)).toBeLessThan(400);
    await expect(page.locator("#search-sheet")).toHaveCount(0);
  });

  test("an upward swipe on the grabber closes the sheet, and so does a tap outside", async ({
    page,
  }) => {
    await page.goto("/");
    await expect(page.locator("#grid [data-tag-id]").first()).toBeVisible();

    await touchDrag(page, await stripCentre(page), OPEN_AT + 40);
    const sheet = page.locator("#search-sheet");
    await expect(sheet).toBeVisible();
    await settledPanel(page);

    const grip = await sheet.locator("[data-testid=sheet-grip]").boundingBox();
    if (!grip) throw new Error("the sheet has no grabber to swipe");
    await touchDrag(page, { x: grip.x + grip.width / 2, y: grip.y + grip.height / 2 }, -140);
    await expect(sheet).toHaveCount(0, { timeout: 2000 });

    // Opened again, this time left by a tap above the panel.
    await touchDrag(page, await stripCentre(page), OPEN_AT + 40);
    await expect(sheet).toBeVisible();
    await page.locator("#search-sheet").click({ position: { x: 10, y: 10 } });
    await expect(sheet).toHaveCount(0);
  });

  test("the ? overlay lists the gesture, since no key can describe it", async ({ page }) => {
    await page.goto("/");
    await expect(page.locator("#grid [data-tag-id]").first()).toBeVisible();

    await page.keyboard.press("?");
    const overlay = page.locator("#shortcut-map");
    await expect(overlay).toBeVisible();
    await expect(overlay).toContainText("Pull down on the grid");
  });
});

test.describe("pull affordance on a fine pointer", () => {
  test.skip(({ hasTouch }) => hasTouch, "a touch project is the one that should render it");

  test("no pull strip is rendered where the pointer is fine", async ({ page }) => {
    await page.goto("/");
    await expect(page.locator("#grid [data-tag-id]").first()).toBeVisible();

    expect(
      await page.evaluate(() => window.matchMedia("(pointer: fine)").matches),
      "this project's pointer is not fine, so the assertion below would prove nothing",
    ).toBe(true);
    await expect(page.locator("#pull")).toHaveCount(0);
  });
});
