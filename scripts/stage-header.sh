#!/usr/bin/env bash
# scripts/stage-header.sh — prepend fate-marker headers to staged files.
#
# REMOVE-WHEN: rewrite-staging/ is fully drained and deleted (end of whichever
# phase absorbs the last staged module). Delete in the same commit as the final
# staging teardown.
#
# Usage: stage-header.sh <dest-crate-path> <origin-path> <phase-id> <reshape-note> <file>
set -euo pipefail
dest_crate="$1"; origin_path="$2"; phase="$3"; reshape="$4"; file="$5"
header="// MOVING TO: ${dest_crate}\n// ORIGIN: ${origin_path}\n// PHASE: ${phase}\n// RESHAPE: ${reshape}\n//\n// This file is retained verbatim for reference during the v3 foundation rewrite.\n// It does not compile in this location; rewrite-staging/ is not a cargo workspace member.\n\n"
tmp=$(mktemp)
printf '%b' "$header" > "$tmp"
cat "$file" >> "$tmp"
mv "$tmp" "$file"
