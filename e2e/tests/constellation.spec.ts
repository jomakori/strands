import { expect, test } from "@playwright/test";

const uuid = (n: number) => `00000000-0000-4000-8000-00000000000${n}`;

const PROFILE = {
  id: uuid(9),
  email: "strands@example.test",
  name: "Strands User",
  provider: "google",
};

const CATEGORY = uuid(8);

const TAGS = [
  { id: uuid(1), name: "Coffee", category_id: CATEGORY, thread_count: 4, created_at: "2026-11-01T09:00:00Z" },
  { id: uuid(2), name: "Lisbon", category_id: CATEGORY, thread_count: 3, created_at: "2026-11-02T09:00:00Z" },
  { id: uuid(3), name: "Notebooks", category_id: CATEGORY, thread_count: 2, created_at: "2026-11-03T09:00:00Z" },
  // A tag the judge never paired with anything: it must still render, and say so.
  { id: uuid(6), name: "Sourdough", category_id: CATEGORY, thread_count: 1, created_at: "2026-11-04T09:00:00Z" },
];

const LINKS = [
  {
    id: uuid(4),
    a_tag_id: uuid(1),
    a_tag_name: "Coffee",
    b_tag_id: uuid(2),
    b_tag_name: "Lisbon",
    score: 82,
    rationale: "Both keep surfacing in the same trip threads.",
    judged_at: "2026-11-10T12:00:00Z",
  },
  {
    id: uuid(5),
    a_tag_id: uuid(1),
    a_tag_name: "Coffee",
    b_tag_id: uuid(3),
    b_tag_name: "Notebooks",
    score: 31,
    rationale: "Shared twice, but about buying gear rather than a place.",
    judged_at: "2026-11-10T12:00:01Z",
  },
];

async function openConstellation(page: any, links: unknown[]) {
  await page.route("**/api/v1/**", async (route: any) => {
    const path = new URL(route.request().url()).pathname;
    const json = (body: unknown) =>
      route.fulfill({
        status: 200,
        contentType: "application/json",
        body: JSON.stringify(body),
      });

    if (path.endsWith("/categories")) return json([]);
    if (path.endsWith("/me")) return json(PROFILE);
    if (path.endsWith("/tags")) return json(TAGS);
    if (path.endsWith("/links")) return json(links);
    return route.fulfill({ status: 404, body: "unexpected request" });
  });

  // Client-side navigation: the static bundle server has no `/constellation` file.
  await page.goto("/");
  await page.locator("#constellation-link").click();
  await expect(page).toHaveURL(/\/constellation$/);
}

test.describe("constellation", () => {
  test("draws every tag as a node and explains the selected one", async ({
    page,
  }, testInfo) => {
    await openConstellation(page, LINKS);

    const nodes = page.locator(".constellation-node");
    await expect(nodes).toHaveCount(TAGS.length);
    await expect(page.locator(".constellation-edges line")).toHaveCount(
      LINKS.length,
    );

    // A tag with no judged edge is on the graph and admits it.
    const lonely = page.locator('button[aria-label="Tag Sourdough"]');
    await expect(lonely).toBeVisible();
    await expect(lonely.locator(".constellation-node-empty")).toHaveText(
      "nothing linked yet",
    );

    // Selection by tap must produce the rationale for that node, including
    // the pair the model scored lower.
    const coffee = page.locator('button[aria-label="Tag Coffee"]');
    await coffee.click();
    await expect(coffee).toHaveAttribute("aria-pressed", "true");

    const rationale = page.locator(".constellation-rationale");
    await expect(rationale.locator("h2")).toHaveText("Coffee");
    await expect(rationale.locator("li")).toHaveCount(2);
    await expect(rationale).toContainText("Coffee ↔ Lisbon");
    await expect(rationale).toContainText("82");
    await expect(rationale).toContainText("trip threads");

    // Keyboard parity: Enter toggles the same selection off.
    await coffee.focus();
    await page.keyboard.press("Enter");
    await expect(coffee).toHaveAttribute("aria-pressed", "false");
    await expect(rationale).toContainText("Select a tag");

    const { clientWidth, scrollWidth } = await page.evaluate(() => ({
      clientWidth: document.documentElement.clientWidth,
      scrollWidth: document.documentElement.scrollWidth,
    }));
    expect(
      scrollWidth,
      `constellation scrolls horizontally: content is ${scrollWidth}px wide in a ${clientWidth}px viewport`,
    ).toBeLessThanOrEqual(clientWidth);

    await page.screenshot({
      path: `captures/${testInfo.project.name}-constellation.png`,
      fullPage: true,
    });
  });

  test("invites judging when no pair has been scored yet", async ({
    page,
  }, testInfo) => {
    await openConstellation(page, []);

    await expect(page.locator('[data-state="empty"]')).toContainText(
      "Nothing is linked yet",
    );
    await expect(page.getByRole("button", { name: "Judge links" })).toBeVisible();
    await expect(page.locator(".constellation-node")).toHaveCount(0);

    await page.screenshot({
      path: `captures/${testInfo.project.name}-constellation-empty.png`,
      fullPage: true,
    });
  });
});
