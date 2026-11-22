#!/usr/bin/env python3
"""Pin the strands production image tag in the GitOps umbrella values."""
import sys

path, tag = sys.argv[1], sys.argv[2]
lines = open(path).read().splitlines(keepends=True)


def indent(line):
    return len(line) - len(line.lstrip())


start = next(k for k, line in enumerate(lines) if line.rstrip("\n") == "strands:")
hits = []
for k in range(start + 1, len(lines)):
    line = lines[k]
    if line.strip() and indent(line) == 0:
        break
    if line.rstrip("\n").rstrip() == "    production:":
        for j in range(k + 1, len(lines)):
            candidate = lines[j]
            if candidate.strip() and indent(candidate) <= 4:
                break
            if candidate.strip().startswith("tag:"):
                hits.append(j)
                break

assert len(hits) == 1, f"expected exactly one strands production tag, found {len(hits)}"
lines[hits[0]] = '      tag: "' + tag + '"\n'
open(path, "w").write("".join(lines))
print("pinned", tag)
