import { expect, test, type Page, type TestInfo } from "@playwright/test";
import AxeBuilder from "@axe-core/playwright";
import fs from "node:fs";
import path from "node:path";

// Ticket T18b: an automated accessibility measurement for the two surfaces that ship, run
// against the release bundle the visual job serves.
//
// axe-core alone is not enough. Every text node in this app sits on a gradient, so axe
// reports `color-contrast` as *incomplete* and would score 100 without ever comparing a
// foreground to a background; and axe cannot see a focus ring or the order Tab moves in. So
// each view is measured three ways — axe's rules, a computed contrast pass over the token
// palette (support/a11y-measure.js), and a keyboard pass — and the score is the weighted
// share of those checks that pass.
//
// Weights follow Lighthouse's shape (critical 10, serious 7, moderate 3, minor 1), so one
// critical or serious failure costs about 5-7% of a rule set: the 95 gate is a real gate.
const THRESHOLD = 95;
const WEIGHT: Record<string, number> = { critical: 10, serious: 7, moderate: 3, minor: 1 };

// axe has no rule for these, and its `label` rule accepts a placeholder, so they are
// measured directly. Each carries the weight of a critical rule (10 = ~6% of a view's
// rule set): a structural miss on this list fails the gate on its own, which is the point.
const OWN_WEIGHT = { contrast: 10, focusVisible: 10, focusOrder: 10, listSemantics: 10, accessibleName: 10 };

type Finding = { id: string; weight: number; pass: boolean; detail: string };

type Sample = {
  target: string;
  ratio: number;
  required: number;
  size: number;
  color: string;
  background: string;
};

type KeyboardReport = {
  samples: number;
  failures: Sample[];
  domCount: number;
  stops: { index: number; path: string; label: string; ring: boolean; note: string }[];
  missing: string[];
  unreachable: string[];
  unnamed: string[];
  tabOrder: string[];
  orderJumps: string[];
  layoutJumps: string[];
};

type Measurement = {
  project: string;
  view: "grid" | "tag-card";
  score: number;
  violations: number;
  nodes: number;
  incomplete: number;
  rules: { id: string; impact: string | null; help: string; nodes: string[] }[];
  contrast: { samples: number; failures: Sample[] };
  focus: { focusables: number; missing: string[]; unreachable: string[]; unnamed: string[]; order: string[] };
  findings: Finding[];
};

const weight = (impact: string | null | undefined) => WEIGHT[impact ?? "minor"] ?? 1;

/** Paint the measurement helpers into the page; they must be re-injected after a navigation. */
async function inject(page: Page) {
  await page.addScriptTag({ path: path.join(__dirname, "..", "support", "a11y-measure.js") });
}

/** Press Tab until the cycle repeats, so every ring is measured under real keyboard focus. */
async function walkKeyboard(page: Page, expected: number) {
  await page.evaluate(() => (document.activeElement as HTMLElement | null)?.blur());
  // Two extra presses: the recorder stops itself on the first repeat, and the trailing ones
  // prove the cycle closed rather than the page running out of controls.
  for (let step = 0; step < expected + 2; step += 1) {
    await page.keyboard.press("Tab");
  }
}

async function measure(page: Page, view: Measurement["view"], project: string): Promise<Measurement> {
  // axe runs its full rule set — WCAG plus best practice — because the ticket's structural
  // items (landmarks, heading order, list semantics) are best-practice rules, not WCAG ones.
  const axe = await new AxeBuilder({ page }).analyze();

  await inject(page);
  const domCount: number = await page.evaluate(() => {
    (window as any).__a11y.start();
    return (window as any).__a11y.domCount();
  });
  await walkKeyboard(page, domCount);
  const keyboard: KeyboardReport = await page.evaluate(() => (window as any).__a11y.report());

  // axe's rule list can name the same rule twice (a rule may report both passes and
  // violations-shaped results); dedupe by id so one rule carries one weight.
  const seen = new Set<string>();
  const rules = [...axe.passes, ...axe.violations].filter((rule) => {
    if (seen.has(rule.id)) return false;
    seen.add(rule.id);
    return true;
  });
  const totalWeight = rules.reduce((sum, rule) => sum + weight(rule.impact), 0);
  const failedWeight = axe.violations.reduce((sum, rule) => sum + weight(rule.impact), 0);

  const listSemantics = await page.evaluate(() => {
    const grid = document.querySelector("#grid");
    if (!grid) return { ok: true, detail: "no grid" };
    const list = grid.matches("ul,ol") || grid.getAttribute("role") === "list";
    const items =
      grid.children.length === 0 ||
      Array.from(grid.children).every((child) => child.matches("li") || child.getAttribute("role") === "listitem");
    const role = grid.getAttribute("role") ? `[role=${grid.getAttribute("role")}]` : "";
    return { ok: list && items, detail: `${grid.tagName.toLowerCase()}${role} with ${grid.children.length} child element(s)` };
  });

  const orderProblems = [...keyboard.orderJumps, ...keyboard.layoutJumps];
  const focusMisses = [...keyboard.missing, ...keyboard.unreachable];

  const findings: Finding[] = [
    ...rules.map((rule) => ({
      id: `axe:${rule.id}`,
      weight: weight(rule.impact),
      pass: !axe.violations.some((violation) => violation.id === rule.id),
      detail: rule.id,
    })),
    {
      id: "contrast",
      weight: OWN_WEIGHT.contrast,
      pass: keyboard.failures.length === 0,
      detail: `${keyboard.samples} text sample(s) measured, ${keyboard.failures.length} below 4.5:1 (or 3:1 for large text)`,
    },
    {
      id: "focus-visible",
      weight: OWN_WEIGHT.focusVisible,
      pass: focusMisses.length === 0,
      detail: `${keyboard.stops.length}/${keyboard.domCount} control(s) reached by Tab, ${focusMisses.length} without a visible ring`,
    },
    {
      id: "focus-order",
      weight: OWN_WEIGHT.focusOrder,
      pass: orderProblems.length === 0,
      detail: `${keyboard.stops.length} tab stop(s), ${orderProblems.length} out of visual order`,
    },
    {
      id: "accessible-name",
      weight: OWN_WEIGHT.accessibleName,
      pass: keyboard.unnamed.length === 0,
      detail: `${keyboard.domCount} control(s) checked, ${keyboard.unnamed.length} with no accessible name (placeholder does not count)`,
    },
    {
      id: "list-semantics",
      weight: OWN_WEIGHT.listSemantics,
      pass: listSemantics.ok,
      detail: listSemantics.detail,
    },
  ];

  const ownWeight = findings.reduce((sum, finding) => sum + finding.weight, 0);
  const ownFailed = findings.filter((finding) => !finding.pass).reduce((sum, finding) => sum + finding.weight, 0);
  const score = totalWeight + ownWeight === 0 ? 100 : 100 * (1 - (failedWeight + ownFailed) / (totalWeight + ownWeight));

  return {
    project,
    view,
    score: Math.round(score * 10) / 10,
    violations: axe.violations.length,
    nodes: axe.violations.reduce((sum, rule) => sum + rule.nodes.length, 0),
    incomplete: axe.incomplete.length,
    rules: axe.violations.map((rule) => ({
      id: rule.id,
      impact: rule.impact ?? null,
      help: rule.help,
      nodes: rule.nodes.map((node) => `${node.target.join(" ")} :: ${node.html.slice(0, 160)}`),
    })),
    contrast: { samples: keyboard.samples, failures: keyboard.failures },
    focus: {
      focusables: keyboard.domCount,
      missing: keyboard.missing,
      unreachable: keyboard.unreachable,
      unnamed: keyboard.unnamed,
      order: keyboard.tabOrder,
    },
    findings,
  };
}

function record(testInfo: TestInfo, measured: Measurement) {
  const dir = path.join(process.cwd(), "a11y");
  fs.mkdirSync(dir, { recursive: true });
  fs.writeFileSync(path.join(dir, `${measured.project}-${measured.view}.json`), JSON.stringify(measured, null, 2));

  // The run says what it measured, not just whether it passed.
  const lines = [
    `${measured.project} ${measured.view}: accessibility ${measured.score} (threshold ${THRESHOLD})`,
    `  axe: ${measured.violations} violation(s) over ${measured.nodes} node(s), ${measured.incomplete} rule(s) axe could not decide`,
    `  contrast: ${measured.contrast.samples} text sample(s), ${measured.contrast.failures.length} failing`,
    `  focus: ${measured.focus.focusables} control(s), ${measured.focus.missing.length} without a ring, ${measured.focus.unreachable.length} unreachable by Tab`,
  ];
  for (const rule of measured.rules) {
    lines.push(`  VIOLATION ${rule.id} [${rule.impact}] — ${rule.help}`);
    for (const node of rule.nodes) lines.push(`    ${node}`);
  }
  for (const failure of measured.contrast.failures) {
    lines.push(`  CONTRAST ${failure.ratio}:1 (needs ${failure.required}:1) ${failure.target} — ${failure.color} on ${failure.background} at ${failure.size}px`);
  }
  for (const gap of measured.focus.missing) lines.push(`  FOCUS ${gap}`);
  for (const gap of measured.focus.unreachable) lines.push(`  UNREACHABLE ${gap}`);
  for (const gap of measured.focus.unnamed) lines.push(`  UNNAMED ${gap}`);
  for (const finding of measured.findings.filter((entry) => !entry.pass)) {
    lines.push(`  FAILED ${finding.id} — ${finding.detail}`);
  }
  console.log(lines.join("\n"));

  expect(
    measured.score,
    `${measured.view} accessibility ${measured.score} < ${THRESHOLD}; failed: ` +
      measured.findings
        .filter((finding) => !finding.pass)
        .map((finding) => `${finding.id} (${finding.detail})`)
        .join("; "),
  ).toBeGreaterThanOrEqual(THRESHOLD);
}

/** Serve a pending proposal so the summary form (and its inputs) is in the DOM. */
async function stubPendingSummary(page: Page) {
  const version = {
    id: "00000000-0000-4000-8000-000000000101",
    tag_id: "00000000-0000-4000-8000-000000000001",
    version: 1,
    body: "Placeholder proposal.",
    guide: { kind: "shorter" },
    model_alias: "strands.summary",
    state: "proposed",
    created_at: "2026-03-01T09:00:00Z",
  };
  await page.route("**/api/v1/tags/*/summary/history", (route) =>
    route.fulfill({ json: { versions: [version], approved_version_id: null } }),
  );
}

test.describe("accessibility", () => {
  test("the grid measures at or above the bar", async ({ page }, testInfo) => {
    await page.goto("/");
    await expect(page.locator("#grid [data-tag-id]")).toHaveCount(16);

    record(testInfo, await measure(page, "grid", testInfo.project.name));
  });

  test("the tag card view measures at or above the bar", async ({ page }, testInfo) => {
    await stubPendingSummary(page);
    await page.goto("/");
    await page.locator("#grid [data-tag-id]").first().click();
    await expect(page.locator("#tag-detail")).toBeVisible();
    await expect(page.locator("#detail-name")).not.toBeEmpty();

    // The card's own form is part of this view: open the editor so its textarea is measured,
    // and pick the freeform guide so its text input is measured too.
    await page.locator("#summary-edit-start").click();
    await expect(page.locator("#summary-edit")).toBeVisible();
    await page.getByRole("button", { name: "Freeform" }).click();
    await expect(page.locator("#summary-instruction")).toBeVisible();

    record(testInfo, await measure(page, "tag-card", testInfo.project.name));
  });
});
