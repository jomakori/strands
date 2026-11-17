#!/usr/bin/env bash
# WASM gzip budget (T18).
#
# `gzip -n` keeps the header out of the count, so the same bytes always measure the same.
# The bundle ships compressed, so the number that matters to a reader on a phone is
# the gzip size, not the size on disk. This script measures the real release
# artifact — the wasm the browser downloads — and fails when it goes over budget.
# CI runs exactly this file, so a local run and a CI run cannot disagree.
#
# Usage: scripts/wasm-budget.sh [wasm-or-dir ...]
#   Default target: artifacts/strands-web*.wasm
#   Budget:        WASM_GZIP_BUDGET_BYTES (default 2621440 = 2.5 MiB)
set -euo pipefail

readonly BUDGET="${WASM_GZIP_BUDGET_BYTES:-2621440}"
readonly SUMMARY_FILE="${GITHUB_STEP_SUMMARY:-}"

# Every wasm the budget applies to: an argument, a directory to scan, or the
# default artifacts glob. A missing artifact is a failure, not a zero.
targets=()
for candidate in "${@:-artifacts}"; do
  if [ -d "${candidate}" ]; then
    while IFS= read -r found; do
      targets+=("${found}")
    done < <(find "${candidate}" -maxdepth 1 -name 'strands-web*.wasm' -type f | sort)
  elif [ -f "${candidate}" ]; then
    targets+=("${candidate}")
  fi
done

if [ "${#targets[@]}" -eq 0 ]; then
  echo "::error::no strands-web*.wasm artifact to measure (looked in: ${*:-artifacts})" >&2
  exit 1
fi

rows=()
failed=0
for wasm in "${targets[@]}"; do
  raw_bytes=$(wc -c <"${wasm}" | tr -d ' ')
  gzip_bytes=$(gzip -9 -n -c "${wasm}" | wc -c | tr -d ' ')
  headroom=$(( BUDGET - gzip_bytes ))
  mib=$(awk -v b="${gzip_bytes}" 'BEGIN { printf "%.2f", b / 1048576 }')
  raw_mib=$(awk -v b="${raw_bytes}" 'BEGIN { printf "%.2f", b / 1048576 }')
  used_pct=$(awk -v b="${gzip_bytes}" -v budget="${BUDGET}" 'BEGIN { printf "%.1f", 100 * b / budget }')
  verdict="pass"
  if [ "${gzip_bytes}" -gt "${BUDGET}" ]; then
    verdict="**fail**"
    failed=1
  fi
  rows+=("| \`$(basename "${wasm}")\` | ${raw_mib} MiB | ${mib} MiB | ${used_pct}% | ${headroom} B | ${verdict} |")
  echo "${wasm}: raw ${raw_bytes} B, gzip ${gzip_bytes} B, budget ${BUDGET} B, headroom ${headroom} B -> ${verdict}"
done

budget_mib=$(awk -v b="${BUDGET}" 'BEGIN { printf "%.2f", b / 1048576 }')
summary="### WASM gzip budget

Budget: ${budget_mib} MiB (${BUDGET} bytes), measured on the release artifact the
browser downloads.

| Artifact | Raw | Gzip | Budget used | Headroom | Result |
| --- | --- | --- | --- | --- | --- |
$(printf '%s\n' "${rows[@]}")"

if [ -n "${SUMMARY_FILE}" ]; then
  printf '%s\n' "${summary}" >>"${SUMMARY_FILE}"
else
  printf '%s\n' "${summary}"
fi

if [ "${failed}" -ne 0 ]; then
  echo "::error::the wasm bundle is over its ${BUDGET} byte gzip budget" >&2
  exit 1
fi
