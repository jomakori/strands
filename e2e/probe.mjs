import { chromium } from '@playwright/test';
const b = await chromium.launch();
const page = await b.newPage({ viewport: { width: 360, height: 800 } });
const versions = [{id:"11111111-1111-4111-8111-111111111111",tag_id:"9b83f584-7f85-5848-a1cb-3bea4729b1e5",version:1,body:"Proposed text.",guide:{kind:"shorter"},model_alias:"m",state:"proposed",created_at:"2026-03-10T09:00:00Z"}];
await page.route("**/api/v1/tags/*/summary/history", r => r.fulfill({json:{versions, approved_version_id: null}}));
page.on("console", m => console.log("C:", m.text()));
page.on("pageerror", e => console.log("ERR:", e.message));
await page.goto("http://127.0.0.1:4173/");
await page.locator("#grid [data-tag-id]").first().click();
await page.waitForSelector("#summary-pending");
await page.evaluate(() => { window.__probe = []; document.addEventListener('keydown', e => window.__probe.push(e.key)); });
await page.keyboard.press("a");
await page.waitForTimeout(300);
console.log("probe:", await page.evaluate(() => window.__probe));
// does the card react to a keydown dispatched directly on the section?
await page.evaluate(() => document.querySelector('#summary-card').dispatchEvent(new KeyboardEvent('keydown', {key:'h', bubbles:true})));
await page.waitForTimeout(300);
console.log("history open text:", await page.locator("#summary-card").innerText().then(t => t.includes("Hide (H)")));
await b.close();
