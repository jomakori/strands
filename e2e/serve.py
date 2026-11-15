#!/usr/bin/env python3
"""Serve the built bundle locally, with the MIME types a browser needs for Wasm."""

import functools
import http.server
import mimetypes
import sys

mimetypes.add_type("application/wasm", ".wasm")
mimetypes.add_type("text/javascript", ".js")
mimetypes.add_type("text/css", ".css")


def main() -> int:
    if len(sys.argv) != 3:
        print("usage: serve.py <bundle-dir> <port>", file=sys.stderr)
        return 2

    bundle_dir, port = sys.argv[1], int(sys.argv[2])
    handler = functools.partial(
        http.server.SimpleHTTPRequestHandler, directory=bundle_dir
    )
    with http.server.ThreadingHTTPServer(("127.0.0.1", port), handler) as server:
        print(f"serving {bundle_dir} on http://127.0.0.1:{port}", flush=True)
        server.serve_forever()
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
