#!/usr/bin/env python3
"""Serve the built bundle locally, with the MIME types a browser needs for Wasm.

The bundle asks one question before it draws anything: `GET /api/v1/me`, whose answer decides
whether the visitor gets the app or the sign-in gate (T24). There is no database here, so this
fixture stands in for that one call the way the real server does — a session cookie in the shape
the server issues is answered with the profile, anything else with 401. `/auth/google` answers
the way the real start route ends: back to `next` with a session cookie, so the sign-in round
trip can be driven in a browser.

Two things are deliberately absent. Every other `/api` path stays a 404 (as JSON), because the
specs that cover failure and retry stub those endpoints at the network boundary themselves — a
fixture that answered them would hide the state they are testing. And nothing here decodes a
token: the fixture trusts a cookie value, the app never does, which is the point of the probe.

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
import urllib.parse

mimetypes.add_type("application/wasm", ".wasm")
mimetypes.add_type("text/javascript", ".js")
mimetypes.add_type("text/css", ".css")

SESSION_COOKIE = "strands_session"
#: A session id in the shape the server issues, so the fixture accepts it (server session.rs).
SESSION_ID = "5f0c1f7a-0000-4000-8000-0000000000a1"
UUID = re.compile(r"\A[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}\Z")

PROFILE = {
    "id": "5f0c1f7a-0000-4000-8000-0000000000b1",
    "email": "strands@example.test",
    "name": "Strands User",
    "provider": "google",
}


class BundleHandler(http.server.SimpleHTTPRequestHandler):
    """Static bundle, plus the handful of auth answers the bundle needs to boot."""

    def do_GET(self) -> None:  # noqa: N802 - the method name is the HTTP verb
        path = urllib.parse.urlparse(self.path).path
        if path == "/api/v1/me":
            return self._session_probe()
        if path == "/auth/google":
            return self._start_sign_in()
        if path.startswith(("/api/", "/auth/")):
            return self._json(
                404, {"error": "endpoint not served by the fixture bundle server"}
            )
        if self._serves_file(path):
            return super().do_GET()
        return self._index()

    def do_POST(self) -> None:  # noqa: N802
        return self._json(
            404, {"error": "endpoint not served by the fixture bundle server"}
        )

    # -- the fixture's two answers ---------------------------------------------------

    def _session_probe(self) -> None:
        """`GET /api/v1/me`: 200 for a session cookie, 401 for everything else."""
        if self._has_session():
            return self._json(200, PROFILE)
        return self._json(401, {"error": "unauthenticated"})

    def _start_sign_in(self) -> None:
        """`GET /auth/google`: the provider dance, skipped, ending where the server ends it."""
        query = urllib.parse.parse_qs(urllib.parse.urlparse(self.path).query)
        return_to = (query.get("next") or [""])[0]
        # Only a local path survives, which is the rule the server applies too.
        if not return_to.startswith("/") or return_to.startswith("//"):
            return_to = "/"
        self.send_response(303)
        self.send_header("Location", return_to)
        self.send_header(
            "Set-Cookie",
            f"{SESSION_COOKIE}={SESSION_ID}; Path=/; HttpOnly; SameSite=Lax",
        )
        self.send_header("Cache-Control", "no-store")
        self.send_header("Content-Length", "0")
        self.end_headers()

    # -- plumbing --------------------------------------------------------------------

    def _has_session(self) -> bool:
        header = self.headers.get("Cookie") or ""
        for part in header.split(";"):
            name, _, value = part.strip().partition("=")
            if name == SESSION_COOKIE:
                return bool(UUID.match(value.strip()))
        return False

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

    def _json(self, status: int, body: dict) -> None:
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
