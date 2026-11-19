import type { BrowserContextOptions, Page } from "@playwright/test";

// One place the suite keeps its view of "a browser with a live session" (T24).
//
// The bundle never reads a token: it asks `GET /api/v1/me` and believes only that answer, so a
// test that wants to be signed in has to look like a browser holding the cookie the server set,
// and be answered by the server standing in behind the bundle (`serve.py`). The id here is in
// the shape the real server issues (`crates/server/src/auth/session.rs`); nothing in the app
// parses it.

/** The cookie the server sets on a completed sign-in. */
export const SESSION_COOKIE = "strands_session";

/** A session id `serve.py` accepts — and the app does not trust for a moment. */
export const SESSION_ID = "5f0c1f7a-0000-4000-8000-0000000000a1";

/** The second seeded account, with its own disjoint rows (T25's isolation case). */
export const SECOND_SESSION_ID = "5f0c1f7a-0000-4000-8000-0000000000a2";

export const THIRD_SESSION_ID = "5f0c1f7a-0000-4000-8000-0000000000a3";

/** The identity `/api/v1/me` answers with, as `strands_api_types::Profile`. */
export const PROFILE = {
  id: "5f0c1f7a-0000-4000-8000-0000000000b1",
  email: "strands@example.test",
  name: "Strands User",
  provider: "google",
};

/** The identity the second seeded account gets back from the same probe. */
export const SECOND_PROFILE = {
  id: "5f0c1f7a-0000-4000-8000-0000000000b2",
  email: "other@example.test",
  name: "Other User",
  provider: "google",
};

type StorageState = NonNullable<BrowserContextOptions["storageState"]>;

/** The cookie a signed-in browser holds, as Playwright sends it. */
export const sessionCookie = {
  name: SESSION_COOKIE,
  value: SESSION_ID,
  domain: "127.0.0.1",
  path: "/",
  expires: -1,
  httpOnly: true,
  secure: false,
  sameSite: "Lax" as const,
};

/** A browser that has completed sign-in and is holding the session cookie. */
export const signedIn: StorageState = {
  cookies: [sessionCookie],
  origins: [],
};

/** A browser that has never signed in: no cookies at all. */
export const signedOut: StorageState = { cookies: [], origins: [] };

/** A browser signed in as the account behind `sessionId` (the cookie is the only difference). */
export function signedInAs(sessionId: string): StorageState {
  return { cookies: [{ ...sessionCookie, value: sessionId }], origins: [] };
}

/** What the page saw, counted from its first frame. */
export type GateWatch = { gate: boolean; probing: boolean; frames: number };

/**
 * Record whether each pre-session screen ever reached the DOM.
 *
 * "Never flashes the gate" is a claim about frames that a normal assertion cannot make: by the
 * time `expect` runs, a flashed screen is already gone. So this watches from the first frame of
 * every document — a mutation observer for anything inserted, and a frame callback for the gap
 * a mutation batch can leave — and the spec reads the tally back afterwards.
 */
export async function watchGate(page: Page): Promise<void> {
  await page.addInitScript(() => {
    const seen = { gate: false, probing: false, frames: 0 };
    (window as unknown as { __gateWatch: typeof seen }).__gateWatch = seen;

    const scan = () => {
      seen.frames += 1;
      if (document.getElementById("gate")) seen.gate = true;
      if (document.getElementById("gate-probing")) seen.probing = true;
    };

    new MutationObserver(scan).observe(document, { childList: true, subtree: true });
    const tick = () => {
      scan();
      if (!seen.gate) requestAnimationFrame(tick);
    };
    requestAnimationFrame(tick);
  });
}

/** The tally for the document currently loaded. */
export function readGateWatch(page: Page): Promise<GateWatch> {
  return page.evaluate(
    () => (window as unknown as { __gateWatch: GateWatch }).__gateWatch,
  );
}
