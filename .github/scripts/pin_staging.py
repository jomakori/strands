#!/usr/bin/env python3
"""Point the strands staging environment at one pull request, or at nothing.

Two values decide whether `strands-staging` exists and which image it runs, and
they are written together so a tag can never render against a stale gate:

  apps/argocd-appset/values.yaml   strands.environment.staging.enable
  apps/helm/values.yaml            strands.environments.staging.tag

Usage: pin_staging.py <gitops-checkout> [<pr-number>]

With a pull request number the gate turns on and the tag becomes `pr-<N>`; with
no argument the gate turns off and the tag returns to the `0.0.0` placeholder.
Only those two scalars change — every other byte of both files is preserved,
which keeps the production render identical.
"""
import sys

PLACEHOLDER = "0.0.0"


def indent(line):
    return len(line) - len(line.lstrip())


def block(lines, key, level, lo, hi):
    """Index range of the block nested under `key:` at column `level`."""
    for k in range(lo, hi):
        if lines[k].rstrip("\n") == " " * level + key + ":":
            end = k + 1
            while end < hi:
                candidate = lines[end]
                if candidate.strip() and indent(candidate) <= level:
                    break
                end += 1
            return k, end
    raise SystemExit(f"no {key}: at indent {level}")


def set_scalar(lines, path, name, value):
    """Replace the single `name: value` line under the nested `path` keys."""
    level, lo, hi = 0, 0, len(lines)
    for key in path:
        lo, hi = block(lines, key, level, lo, hi)
        level += 2

    hits = [k for k in range(lo + 1, hi) if lines[k].strip().startswith(name + ":")]
    assert len(hits) == 1, f"expected one {'.'.join(path + [name])}, found {len(hits)}"
    hit = hits[0]
    assert indent(lines[hit]) == level, f"{name}: is not at indent {level}"
    lines[hit] = " " * level + f"{name}: {value}\n"


def main():
    root, pr = sys.argv[1], (sys.argv[2] if len(sys.argv) > 2 else "")
    enable = "true" if pr else "false"
    tag = f'"pr-{pr}"' if pr else f'"{PLACEHOLDER}"'

    targets = {
        "apps/argocd-appset/values.yaml": (["strands", "environment", "staging"], "enable", enable),
        "apps/helm/values.yaml": (["strands", "environments", "staging"], "tag", tag),
    }
    for rel, (path, name, value) in targets.items():
        full = f"{root}/{rel}"
        with open(full) as handle:
            lines = handle.readlines()
        set_scalar(lines, path, name, value)
        with open(full, "w") as handle:
            handle.writelines(lines)
        print(f"{rel}: {'.'.join(path + [name])} = {value}")


main()
