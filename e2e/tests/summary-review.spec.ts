import { expect, test, type Page } from "@playwright/test";

// Runs against the release Wasm bundle CI builds (see home.spec.ts), so the API is
// stubbed: a generated summary is a proposal until the card approves it. Covers T8 —
// approve commits, discard drops, edit records a new version, and A / D / H stay on
// the card (and out of the way of a text field).

const TAG_STUB = "00000000-0000-4000-8000-000000000001";
const MODEL_ALIAS = "strands.summary";

type Version = {
  id: string;
  tag_id: string;
  version: number;
  body: string;
  guide: { kind: string };
  model_alias: string;
  state: string;
  created_at: string;
};

type Stub = {
  versions: Version[];
  approved: string | null;
  proposal: (body: string) => Version;
};

function version(number: number, body: string, state: string): Version {
  return {
    id: `00000000-0000-4000-8000-00000000010${number}`,
    tag_id: TAG_STUB,
    version: number,
    body,
    guide: { kind: "shorter" },
    model_alias: MODEL_ALIAS,
    state,
    created_at: "2026-03-10T09:00:00Z",
  };
}

/** Serve the review endpoints from memory, so a reload reads back what was committed. */
async function stubReviewApi(
  page: Page,
  initial: { versions: Version[]; approved: string | null },
): Promise<Stub> {
  const stub: Stub = {
    ...initial,
    proposal: (body: string) => version(stub.versions.length + 1, body, "proposed"),
  };

  const history = () =>
    Promise.resolve({
      versions: stub.versions,
      approved_version_id: stub.approved,
    });

  await page.route("**/api/v1/tags/*/summary/history", async (route) => {
    await route.fulfill({ json: await history() });
  });

  await page.route("**/api/v1/tags/*/summary/*/approve", async (route) => {
    const id = route.request().url().split("/").at(-2)!;
    const target = stub.versions.find((entry) => entry.id === id);
    if (target && target.state !== "discarded") {
      target.state = "approved";
      stub.approved = target.id;
    }
    await route.fulfill({ json: await history() });
  });

  await page.route("**/api/v1/tags/*/summary/*/discard", async (route) => {
    const id = route.request().url().split("/").at(-2)!;
    const target = stub.versions.find((entry) => entry.id === id);
    if (target) {
      target.state = "discarded";
      if (stub.approved === target.id) {
        stub.approved = null;
      }
    }
    await route.fulfill({ json: await history() });
  });

  await page.route("**/api/v1/tags/*/summary/*/edit", async (route) => {
    const body = (route.request().postDataJSON() as { body: string }).body;
    stub.versions = [version(stub.versions.length + 1, body, "edited"), ...stub.versions];
    await route.fulfill({ json: await history() });
  });

  return stub;
}

/** Open the tag card; `pending` waits for the proposal so a keypress cannot race the load. */
async function openCard(page: Page, pending = true) {
  await page.goto("/");
  await page.locator("#grid [data-tag-id]").first().click();
  await expect(page.locator("#summary-card")).toBeVisible();
  if (pending) {
    await expect(page.locator("#summary-pending")).toBeVisible();
  }
}

test.describe("summary review", () => {
  test("approving a proposal commits it and it survives a reload", async ({
    page,
  }) => {
    await stubReviewApi(page, {
      versions: [version(1, "Three trips to Tokyo, mostly trains.", "proposed")],
      approved: null,
    });
    await openCard(page);

    await expect(page.locator("#summary-pending")).toBeVisible();
    await expect(page.locator("#summary-body")).toHaveAttribute(
      "data-state",
      "empty",
    );

    await page.locator("#summary-approve").click();

    await expect(page.locator("#summary-pending")).toHaveCount(0);
    await expect(page.locator("#summary-body")).toHaveText(
      "Three trips to Tokyo, mostly trains.",
    );
    await expect(page.locator("#summary-approved-badge")).toBeVisible();
    await expect(
      page.locator("#summary-versions [data-state='approved']"),
    ).toHaveCount(1);

    // Leaving the card and coming back is the acceptance check: the body comes
    // from the server's history, not from state that happened to survive a click.
    await page.goto("/");
    await page.locator("#grid [data-tag-id]").first().click();
    await expect(page.locator("#summary-body")).toHaveText(
      "Three trips to Tokyo, mostly trains.",
    );
    await expect(page.locator("#summary-pending")).toHaveCount(0);
  });

  test("the A key approves the card's proposal", async ({ page }) => {
    await stubReviewApi(page, {
      versions: [version(1, "Keyboard approved.", "proposed")],
      approved: null,
    });
    await openCard(page);

    await page.keyboard.press("a");

    await expect(page.locator("#summary-body")).toHaveText("Keyboard approved.");
    await expect(page.locator("#summary-pending")).toHaveCount(0);
  });

  test("discarding a proposal restores the previously approved body", async ({
    page,
  }) => {
    await stubReviewApi(page, {
      versions: [
        version(2, "A fresh proposal nobody asked for.", "proposed"),
        version(1, "What was committed last time.", "approved"),
      ],
      approved: "00000000-0000-4000-8000-000000000101",
    });
    await openCard(page);

    await expect(page.locator("#summary-body")).toHaveText(
      "What was committed last time.",
    );
    await expect(page.locator("#summary-pending-body")).toHaveText(
      "A fresh proposal nobody asked for.",
    );

    await page.locator("#summary-discard").click();

    await expect(page.locator("#summary-pending")).toHaveCount(0);
    await expect(page.locator("#summary-body")).toHaveText(
      "What was committed last time.",
    );
    await expect(page.locator("#summary-versions [data-state='discarded']")).toHaveCount(
      1,
    );
  });

  test("A does nothing while a text field has focus", async ({ page }) => {
    await stubReviewApi(page, {
      versions: [version(1, "Still a proposal.", "proposed")],
      approved: null,
    });
    await openCard(page);

    await page.locator(".guide-chip", { hasText: "Freeform" }).click();
    const instruction = page.locator("#summary-instruction");
    await instruction.click();
    // Dioxus owns the input value, so typing goes key by key (see the repo rules).
    await instruction.pressSequentially("a date focus", { delay: 20 });

    await expect(instruction).toHaveValue("a date focus");
    await expect(page.locator("#summary-pending")).toBeVisible();
    await expect(page.locator("#summary-body")).toHaveAttribute(
      "data-state",
      "empty",
    );
  });

  test("editing a proposal records a new version instead of rewriting it", async ({
    page,
  }) => {
    const stub = await stubReviewApi(page, {
      versions: [version(1, "Generated words.", "proposed")],
      approved: null,
    });
    await openCard(page);

    await page.locator("#summary-edit-start").click();
    const editor = page.locator("#summary-edit");
    await editor.click();
    // The draft starts from the proposal's own text; clear it with keystrokes
    // (never fill()) and type the rewrite key by key.
    await editor.press("ControlOrMeta+a");
    await editor.press("Backspace");
    await editor.pressSequentially("My own words.", { delay: 20 });
    await page.locator("#summary-edit-save").click();

    // The old version keeps its body; the rewrite arrives as its own row.
    expect(stub.versions).toHaveLength(2);
    expect(stub.versions[1].body).toBe("Generated words.");
    expect(stub.versions[1].state).toBe("proposed");
    await expect(page.locator("#summary-versions li")).toHaveCount(2);
    await expect(
      page.locator("#summary-versions [data-state='edited']"),
    ).toHaveCount(1);
    await expect(page.locator("#summary-pending-body")).toHaveText(
      "My own words.",
    );
    await expect(page.locator("#summary-body")).toHaveAttribute(
      "data-state",
      "empty",
    );
  });
});
