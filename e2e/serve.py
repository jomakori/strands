#!/usr/bin/env python3
"""Serve the built bundle locally, with the MIME types a browser needs for Wasm.

The bundle asks one question before it draws anything: `GET /api/v1/me`, whose answer decides
whether the visitor gets the app or the sign-in gate (T24). There is no database here, so this
module stands in for the server behind both: a session cookie in the shape the server issues is
answered with that user's profile, anything else with 401. `/auth/google` answers the way the
real start route ends: back to `next` with a session cookie, so the sign-in round trip can be
driven in a browser.

Since T25 the grid reads the server, so this file also answers the reads the app makes of it:
`/api/v1/categories`, `/api/v1/tags`, `/api/v1/tags/{id}/threads`, and the writes quick capture
makes (`POST /api/v1/tags`, `POST /api/v1/threads`, archive). The rows are seeded per user — two
users, disjoint tags — and kept in memory, so a note captured in a browser is still there after
a reload and a visitor signed in as one user never sees the other's tags. That in-memory store
is what the CI captures photograph: server rows, not a fixture baked into the bundle.

The specs that cover failure and retry stub those paths at the network boundary themselves, so
their states are theirs to produce; unknown `/api` paths still answer 404 (as JSON) rather than
inventing a row. And nothing here decodes a token: the fixture trusts a cookie value, the app
never does, which is the point of the probe.

Every other path falls back to `index.html` so a deep link boots the app; the deployed server
serves its client-side routes the same way.
"""

import functools
import http.server
import json
import mimetypes
import os
import re
import sys
import threading
import urllib.parse
import uuid
from datetime import datetime, timedelta, timezone

mimetypes.add_type("application/wasm", ".wasm")
mimetypes.add_type("text/javascript", ".js")
mimetypes.add_type("text/css", ".css")

SESSION_COOKIE = "strands_session"
#: A session id in the shape the server issues, so the fixture accepts it (server session.rs).
SESSION_ID = "5f0c1f7a-0000-4000-8000-0000000000a1"
#: The second seeded user: same shape, different rows, so isolation is observable.
SECOND_SESSION_ID = "5f0c1f7a-0000-4000-8000-0000000000a2"
#: The account the writing specs use, so a created row cannot skew another spec's count.
THIRD_SESSION_ID = "5f0c1f7a-0000-4000-8000-0000000000a3"
UUID = re.compile(r"\A[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}\Z")

#: The five categories the server seeds for every account (server `auth/store.rs`).
CATEGORIES = [
    ("Travel", "#2F6F4F", 0),
    ("Tech", "#2F5D8C", 1),
    ("Food", "#8C4A2F", 2),
    ("Reading", "#5B4A8C", 3),
    ("Ideas", "#7A6A2F", 4),
]

#: The seeded rows behind `strands@example.test`: 16 tags across the five categories, each with
#: its threads. Ages are minutes back from the request, so the cards keep saying "3h" instead of
#: drifting into "5w" as the seed file ages.
SEED_USER = {
    "profile": {
        "id": "5f0c1f7a-0000-4000-8000-0000000000b1",
        "email": "strands@example.test",
        "name": "Strands User",
        "provider": "google",
    },
    "tags": [
        (
            "Tokyo",
            "Travel",
            [
                ("The 6 a.m. train is not what the guidebooks promise", 180),
                ("Finding quiet in Shinjuku", 2 * 24 * 60),
                ("Vending machines as infrastructure", 6 * 24 * 60),
            ],
        ),
        (
            "Kyoto",
            "Travel",
            [
                ("Walking the Philosopher's Path at golden hour", 9 * 60),
                ("Moss and maintenance", 4 * 24 * 60),
            ],
        ),
        (
            "Berlin",
            "Travel",
            [
                ("A night that starts at 2 a.m.", 22 * 60),
                ("The Spree at sunrise", 8 * 24 * 60),
            ],
        ),
        ("Lisbon", "Travel", [("Learning to stop on the hills", 3 * 24 * 60)]),
        (
            "language models",
            "Tech",
            [
                ("Reasoning is not the same as understanding", 14),
                ("Small models at the edge", 20 * 60),
                ("Evaluating what we can't easily measure", 5 * 24 * 60),
            ],
        ),
        ("edge computing", "Tech", [("The cost of a round trip", 2 * 60)]),
        ("TypeScript", "Tech", [("Types that say too much", 2 * 24 * 60)]),
        (
            "sourdough",
            "Food",
            [
                ("Why my starter has a name", 45),
                ("Oven spring at altitude", 3 * 24 * 60),
            ],
        ),
        ("ramen", "Food", [("The third bowl is the best one", 7 * 60)]),
        ("night markets", "Food", [("Order one thing at a time", 4 * 24 * 60)]),
        (
            "Moby-Dick",
            "Reading",
            [
                ("The cetology chapters are a feature", 5 * 60),
                ("What the whale is for", 9 * 24 * 60),
            ],
        ),
        ("essays", "Reading", [("The essay is a first draft of attention", 11 * 60)]),
        ("notebooks", "Reading", [("Reading your old marginalia", 6 * 24 * 60)]),
        (
            "slow time",
            "Ideas",
            [
                ("Doing one thing badly, on purpose", 30),
                ("The week has a shape", 4 * 24 * 60),
            ],
        ),
        (
            "questions",
            "Ideas",
            [
                ("What would I do with a free Tuesday", 5 * 60),
                ("Asking better follow-ups", 6 * 24 * 60),
            ],
        ),
        ("margin notes", "Ideas", [("Reading with a pencil is a conversation", 12 * 60)]),
    ],
}

#: The other seeded account (T25's isolation case): three tags, none of them shared with the
#: user above, so "only my tags render" is a claim a browser can falsify.
SEED_SECOND_USER = {
    "profile": {
        "id": "5f0c1f7a-0000-4000-8000-0000000000b2",
        "email": "other@example.test",
        "name": "Other User",
        "provider": "google",
    },
    "tags": [
        ("Sourdough starter", "Food", [("Feeding it twice a day", 90)]),
        ("Bouldering", "Ideas", [("Reading a route before pulling on", 3 * 24 * 60)]),
        ("Night trains", "Travel", [("A sleeper from Vienna", 26 * 60)]),
    ],
}

#: A third account with one tag, used by the specs that write: a capture test's new row must not
#: land in a grid another test is counting, and each spec gets its own server state that way.
SEED_THIRD_USER = {
    "profile": {
        "id": "5f0c1f7a-0000-4000-8000-0000000000b3",
        "email": "sketcher@example.test",
        "name": "Sketcher",
        "provider": "google",
    },
    "tags": [("Sketching", "Ideas", [("A page of thumbs before anything else", 40)])],
}

SESSIONS = {
    SESSION_ID: SEED_USER,
    SECOND_SESSION_ID: SEED_SECOND_USER,
    THIRD_SESSION_ID: SEED_THIRD_USER,
}

#: The live store, one entry per session: seeded on first use, then whatever the browser wrote.
STORE: dict[str, dict] = {}
LOCK = threading.Lock()


def _stable(namespace: str, name: str) -> str:
    """An id that does not change between requests, so a reload addresses the same rows."""
    return str(uuid.uuid5(uuid.NAMESPACE_URL, f"strands/{namespace}/{name}"))


def _now() -> datetime:
    return datetime.now(timezone.utc)


def _iso(moment: datetime) -> str:
    return moment.isoformat().replace("+00:00", "Z")


def _store(session_id: str) -> dict:
    """The rows behind one account, seeded on first touch."""
    with LOCK:
        if session_id in STORE:
            return STORE[session_id]

        seed = SESSIONS[session_id]
        categories = {
            name: {"id": _stable(session_id, f"category/{name}"), "name": name, "color": color, "position": position}
            for name, color, position in CATEGORIES
        }
        tags = {}
        threads = {}
        now = _now()
        for name, category, rows in seed["tags"]:
            tag_id = _stable(session_id, f"tag/{name}")
            tags[tag_id] = {
                "id": tag_id,
                "name": name,
                "category_id": categories[category]["id"],
                "created_at": _iso(now - timedelta(days=30)),
            }
            for index, (title, minutes) in enumerate(rows):
                thread_id = _stable(session_id, f"thread/{name}/{index}")
                threads[thread_id] = {
                    "id": thread_id,
                    "tag_id": tag_id,
                    "title": title,
                    "source_count": 1,
                    "updated_at": _iso(now - timedelta(minutes=minutes)),
                    "archived_at": None,
                }
        STORE[session_id] = {
            "profile": seed["profile"],
            "categories": categories,
            "tags": tags,
            "threads": threads,
        }
        return STORE[session_id]


def _title(body: str) -> str:
    """The thread title the server would derive from a captured note."""
    line = " ".join(body.strip().splitlines()[0].split())
    return line if len(line) <= 80 else line[:77].rstrip() + "…"


class BundleHandler(http.server.SimpleHTTPRequestHandler):
    """Static bundle, plus the API answers the bundle reads and writes."""

    def do_GET(self) -> None:  # noqa: N802 - the method name is the HTTP verb
        path = urllib.parse.urlparse(self.path).path
        if path == "/auth/google":
            return self._start_sign_in()
        if path.startswith(("/api/", "/auth/")):
            return self._api_get(path)
        if self._serves_file(path):
            return super().do_GET()
        return self._index()

    def do_POST(self) -> None:  # noqa: N802
        path = urllib.parse.urlparse(self.path).path
        if not path.startswith("/api/"):
            return self._json(404, {"error": "endpoint not served by the fixture bundle server"})
        return self._api_post(path)

    # -- the API the bundle talks to ------------------------------------------------

    def _api_get(self, path: str) -> None:
        session_id = self._session()
        if session_id is None:
            return self._json(401, {"error": "unauthenticated"})

        store = _store(session_id)
        if path == "/api/v1/me":
            return self._json(200, store["profile"])
        if path == "/api/v1/categories":
            return self._json(200, list(store["categories"].values()))
        if path == "/api/v1/tags":
            return self._json(200, [self._summary(store, tag) for tag in store["tags"].values()])

        tag_id = self._match(path, r"/api/v1/tags/([^/]+)/threads")
        if tag_id is not None:
            if tag_id not in store["tags"]:
                return self._json(404, {"error": "no such tag"})
            return self._json(200, self._threads_of(store, tag_id))

        tag_id = self._match(path, r"/api/v1/tags/([^/]+)")
        if tag_id is not None:
            tag = store["tags"].get(tag_id)
            if tag is None:
                return self._json(404, {"error": "no such tag"})
            return self._json(
                200,
                {
                    "id": tag["id"],
                    "name": tag["name"],
                    "category_id": tag["category_id"],
                    "created_at": tag["created_at"],
                },
            )

        thread_id = self._match(path, r"/api/v1/threads/([^/]+)")
        if thread_id is not None:
            thread = store["threads"].get(thread_id)
            if thread is None:
                return self._json(404, {"error": "no such thread"})
            return self._json(200, self._thread(thread))

        return self._json(404, {"error": "endpoint not served by the fixture bundle server"})

    def _api_post(self, path: str) -> None:
        session_id = self._session()
        if session_id is None:
            return self._json(401, {"error": "unauthenticated"})

        store = _store(session_id)
        body = self._body()

        if path == "/api/v1/tags":
            name = str(body.get("name", "")).strip()
            category_id = str(body.get("category_id", ""))
            if not name:
                return self._json(422, {"error": "name is required"})
            known = {category["id"]: category for category in store["categories"].values()}
            if category_id not in known:
                return self._json(404, {"error": "no such category"})
            existing = next(
                (tag for tag in store["tags"].values() if tag["name"].casefold() == name.casefold()),
                None,
            )
            if existing is None:
                tag_id = str(uuid.uuid4())
                existing = {
                    "id": tag_id,
                    "name": name,
                    "category_id": category_id,
                    "created_at": _iso(_now()),
                }
                store["tags"][tag_id] = existing
            return self._json(201, self._summary(store, existing))

        if path == "/api/v1/threads":
            text = str(body.get("body", ""))
            if not text.strip():
                return self._json(422, {"error": "body is required"})
            tag_id = body.get("tag_id")
            if tag_id is None:
                tag_id = self._inbox(store)
            if tag_id not in store["tags"]:
                return self._json(404, {"error": "no such tag"})
            thread = {
                "id": str(uuid.uuid4()),
                "tag_id": tag_id,
                "title": _title(text),
                "source_count": 1,
                "updated_at": _iso(_now()),
                "archived_at": None,
            }
            store["threads"][thread["id"]] = thread
            return self._json(201, self._thread(thread))

        thread_id = self._match(path, r"/api/v1/threads/([^/]+)/archive")
        if thread_id is not None:
            thread = store["threads"].get(thread_id)
            if thread is None:
                return self._json(404, {"error": "no such thread"})
            # The row leaves the tag's list; the count is derived from the rows, so the grid's
            # numbers follow without a second write.
            thread["archived_at"] = _iso(_now())
            return self._json(204, None)

        return self._json(404, {"error": "endpoint not served by the fixture bundle server"})

    # -- the rows ---------------------------------------------------------------------

    def _summary(self, store: dict, tag: dict) -> dict:
        return {
            "id": tag["id"],
            "name": tag["name"],
            "category_id": tag["category_id"],
            "thread_count": len(self._threads_of(store, tag["id"])),
            "created_at": tag["created_at"],
        }

    def _threads_of(self, store: dict, tag_id: str) -> list:
        rows = [
            self._thread(thread)
            for thread in store["threads"].values()
            if thread["tag_id"] == tag_id and thread["archived_at"] is None
        ]
        return sorted(rows, key=lambda row: row["updated_at"], reverse=True)

    def _thread(self, thread: dict) -> dict:
        return {
            "id": thread["id"],
            "tag_id": thread["tag_id"],
            "title": thread["title"],
            "source_count": thread["source_count"],
            "updated_at": thread["updated_at"],
        }

    def _inbox(self, store: dict) -> str:
        """The per-user Inbox a capture without a tag lands in (server `POST /threads`)."""
        existing = next(
            (tag for tag in store["tags"].values() if tag["name"] == "Inbox"), None
        )
        if existing is None:
            ideas = next(
                category for category in store["categories"].values() if category["position"] == 4
            )
            existing = {
                "id": str(uuid.uuid4()),
                "name": "Inbox",
                "category_id": ideas["id"],
                "created_at": _iso(_now()),
            }
            store["tags"][existing["id"]] = existing
        return existing["id"]

    # -- the fixture's sign-in ----------------------------------------------------------

    def _start_sign_in(self) -> None:
        """`GET /auth/google`: the provider dance, skipped, ending where the server ends it."""
        query = urllib.parse.parse_qs(urllib.parse.urlparse(self.path).query)
        return_to = (query.get("next") or [""])[0]
        # Only a local path survives, which is the rule the server applies too.
        if not return_to.startswith("/") or return_to.startswith("//"):
            return_to = "/"
        # `as=second` picks the other seeded account; the real route would come back with
        # whichever Google account was chosen, and this is the fixture's stand-in for that.
        session_id = SECOND_SESSION_ID if (query.get("as") or [""])[0] == "second" else SESSION_ID
        self.send_response(303)
        self.send_header("Location", return_to)
        self.send_header(
            "Set-Cookie",
            f"{SESSION_COOKIE}={session_id}; Path=/; HttpOnly; SameSite=Lax",
        )
        self.send_header("Cache-Control", "no-store")
        self.send_header("Content-Length", "0")
        self.end_headers()

    # -- plumbing --------------------------------------------------------------------

    def _session(self) -> str | None:
        """The seeded account behind this browser's cookie, when it holds one we know."""
        header = self.headers.get("Cookie") or ""
        for part in header.split(";"):
            name, _, value = part.strip().partition("=")
            if name == SESSION_COOKIE:
                value = value.strip()
                if UUID.match(value) and value in SESSIONS:
                    return value
        return None

    def _match(self, path: str, pattern: str) -> str | None:
        found = re.fullmatch(pattern, path)
        return found.group(1) if found else None

    def _body(self) -> dict:
        length = int(self.headers.get("Content-Length") or 0)
        if length <= 0:
            return {}
        try:
            return json.loads(self.rfile.read(length) or b"{}")
        except json.JSONDecodeError:
            return {}

    def _serves_file(self, path: str) -> bool:
        """Whether this path is a real artifact, not a client-side route."""
        relative = urllib.parse.unquote(path).lstrip("/")
        if not relative:
            return False
        root = os.path.realpath(self.directory)
        candidate = os.path.realpath(os.path.join(root, relative))
        if candidate != root and not candidate.startswith(root + os.sep):
            return False
        return os.path.isfile(candidate)

    def _index(self) -> None:
        """A route the bundle owns: the browser loads the app and the router shows it."""
        with open(os.path.join(self.directory, "index.html"), "rb") as handle:
            body = handle.read()
        self.send_response(200)
        self.send_header("Content-Type", "text/html; charset=utf-8")
        self.send_header("Content-Length", str(len(body)))
        self.send_header("Cache-Control", "no-store")
        self.end_headers()
        self.wfile.write(body)

    def _json(self, status: int, body) -> None:
        if body is None or status == 204:
            self.send_response(204)
            self.send_header("Cache-Control", "no-store")
            self.send_header("Content-Length", "0")
            self.end_headers()
            return
        payload = json.dumps(body).encode()
        self.send_response(status)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(payload)))
        self.send_header("Cache-Control", "no-store")
        self.end_headers()
        self.wfile.write(payload)


def main() -> int:
    if len(sys.argv) != 3:
        print("usage: serve.py <bundle-dir> <port>", file=sys.stderr)
        return 2

    bundle_dir, port = sys.argv[1], int(sys.argv[2])
    handler = functools.partial(BundleHandler, directory=bundle_dir)
    with http.server.ThreadingHTTPServer(("127.0.0.1", port), handler) as server:
        print(f"serving {bundle_dir} on http://127.0.0.1:{port}", flush=True)
        server.serve_forever()
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
