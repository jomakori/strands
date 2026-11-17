// Turns the a11y measurements into a markdown block for $GITHUB_STEP_SUMMARY, and fails the
// step when a view is missing or below the threshold the spec recorded with it.
//
// The spec writes a11y/<project>-<view>.json for every view it measures, so this script never
// recomputes a score — it reports what the browser actually measured.
import fs from "node:fs";
import path from "node:path";

const dir = path.join(process.cwd(), "a11y");
const expected = [
  ["mobile-360", "grid"],
  ["mobile-360", "tag-card"],
  ["desktop-1440", "grid"],
  ["desktop-1440", "tag-card"],
];

const files = fs.existsSync(dir) ? fs.readdirSync(dir).filter((name) => name.endsWith(".json")) : [];
const measurements = files.map((name) => JSON.parse(fs.readFileSync(path.join(dir, name), "utf8")));
const byKey = new Map(measurements.map((entry) => [`${entry.project}-${entry.view}`, entry]));

const missing = expected.filter(([project, view]) => !byKey.has(`${project}-${view}`));
const failing = measurements.filter((entry) => entry.score < entry.threshold);

const rows = expected.map(([project, view]) => {
  const entry = byKey.get(`${project}-${view}`);
  if (!entry) return `| ${project} | ${view} | — | — | not measured |`;
  const failed = entry.findings.filter((finding) => !finding.pass);
  const result = entry.score >= entry.threshold ? "pass" : "**fail**";
  return `| ${project} | ${view} | **${entry.score}** | ${result} | ${entry.violations} axe violation(s); ${failed.length} failing check(s) |`;
});

const lines = [
  "### Accessibility",
  "",
  `Threshold: accessibility >= 95 per view (enforced by the spec; failing findings are listed below).`,
  "",
  "| Viewport | View | Score | Result | Detail |",
  "| --- | --- | --- | --- | --- |",
  ...rows,
  "",
];

for (const entry of measurements.filter((candidate) => candidate.score < candidate.threshold)) {
  lines.push(`<details><summary>${entry.project} ${entry.view} — failing checks</summary>`, "");
  for (const finding of entry.findings.filter((candidate) => !candidate.pass)) {
    lines.push(`- \`${finding.id}\` — ${finding.detail}`);
  }
  for (const rule of entry.rules) {
    lines.push(`- axe \`${rule.id}\` (${rule.impact}) — ${rule.help}`);
    for (const node of rule.nodes) lines.push(`  - \`${node}\``);
  }
  for (const failure of entry.contrast.failures) {
    lines.push(`- contrast ${failure.ratio}:1 < ${failure.required}:1 — \`${failure.target}\` ${failure.color} on ${failure.background}`);
  }
  lines.push("", "</details>", "");
}

const markdown = lines.join("\n");
const summaryPath = process.env.GITHUB_STEP_SUMMARY;
if (summaryPath) fs.appendFileSync(summaryPath, `${markdown}\n`);
else console.log(markdown);

if (missing.length > 0) {
  console.error(`missing measurement(s): ${missing.map(([project, view]) => `${project}/${view}`).join(", ")}`);
  process.exit(1);
}
if (failing.length > 0) {
  console.error(`below threshold: ${failing.map((entry) => `${entry.project}/${entry.view}=${entry.score}`).join(", ")}`);
  process.exit(1);
}
console.log(`accessibility ok: ${measurements.map((entry) => `${entry.project}/${entry.view}=${entry.score}`).join(" ")}`);
