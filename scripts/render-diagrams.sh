#!/usr/bin/env bash
# Renders every D2 diagram in docs/ to an SVG next to its source.
#
#   scripts/render-diagrams.sh          format and render
#   scripts/render-diagrams.sh --check  fail if a source is unformatted, invalid or its SVG is stale
#
# Install the pinned D2 the same way CI does, because the SVG embeds the D2 build's version:
#   go install oss.terrastruct.com/d2@v0.7.2
set -euo pipefail

cd "$(dirname "$0")/.."

check=false
if [[ "${1:-}" == "--check" ]]; then
  check=true
fi

mapfile -t sources < <(find docs -name '*.d2' | sort)
if [[ ${#sources[@]} -eq 0 ]]; then
  echo "no D2 sources under docs/"
  exit 0
fi

for source in "${sources[@]}"; do
  if $check; then
    d2 fmt --check "$source"
  else
    d2 fmt "$source"
  fi
  d2 validate "$source"
  d2 "$source" "${source%.d2}.svg"
done

if $check; then
  # Changed against the index, or never added: either way the committed SVG is not what the source renders.
  stale=$(git diff --name-only -- 'docs/*.svg'; git ls-files --others --exclude-standard -- 'docs/*.svg')
  if [[ -n "$stale" ]]; then
    echo "$stale" >&2
    echo "Rendered SVGs are stale or missing. Run scripts/render-diagrams.sh and commit the result." >&2
    exit 1
  fi
fi
