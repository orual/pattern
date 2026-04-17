#!/usr/bin/env bash
set -euo pipefail

# audit-rewrite-state.sh: enforces v3-foundation.AC1.7–AC1.10 across the
# active workspace crates. Exit non-zero on any violation.

workspace_dirs=(crates/pattern_core crates/pattern_runtime crates/pattern_provider crates/pattern_db)
staging_dir="rewrite-staging"

fail=0

# AC1.7: staging files must carry MOVING TO fate markers.
while IFS= read -r file; do
    if ! head -1 "$file" | grep -q '^// MOVING TO:'; then
        echo "AC1.7 violation: staged file missing MOVING TO header: $file"
        fail=1
    fi
done < <(find "$staging_dir" -type f -name '*.rs')

# AC1.8: unimplemented!()/todo!() in workspace crates must have a phase/AC reference nearby.
while IFS= read -r hit; do
    file="${hit%%:*}"
    line="${hit#*:}"; line="${line%%:*}"
    # Look at the line itself plus the preceding 3 lines for "AC" or "phase"
    context=$(sed -n "$((line-3)),${line}p" "$file")
    if ! echo "$context" | grep -qiE 'phase|AC[0-9]|AC1\.'; then
        echo "AC1.8 violation: unimplemented/todo without phase/AC marker at $file:$line"
        fail=1
    fi
done < <(grep -rnE 'unimplemented!\(|todo!\(' "${workspace_dirs[@]}" || true)

# AC1.9: code regions with fate markers must be syntactically coherent (no dangling markers on nothing).
# Simpler proxy: every MOVING TO / REPLACED BY / MOVING WITHIN CRATE marker inside workspace crates
# must be inside a comment block (not random text).
while IFS= read -r hit; do
    file="${hit%%:*}"
    line="${hit#*:}"; line="${line%%:*}"
    # Confirm the line starts with // (comment).
    content=$(sed -n "${line}p" "$file")
    if ! echo "$content" | grep -qE '^\s*//'; then
        echo "AC1.9 violation: fate marker not inside a comment at $file:$line"
        fail=1
    fi
done < <(grep -rnE '// (MOVING TO|REPLACED BY|MOVING WITHIN CRATE):' "${workspace_dirs[@]}" || true)

# AC1.10: commented-out code blocks in workspace crates fail the audit.
# Heuristic: `//` followed by obvious Rust syntax (pub fn, fn, struct, enum, impl, use crate::, let mut).
# Rustdoc lines (///, //!) are excluded: those are doc-comments, not commented-out
# code. Also excluded: fate markers, explicit Example/doc mentions, and
# "SAFETY:" / "TODO:" / "NOTE:" style annotation prefixes common in well-
# commented Rust code.
while IFS= read -r hit; do
    file="${hit%%:*}"
    line="${hit#*:}"; line="${line%%:*}"
    # Allow fate markers and doc markers; flag everything else.
    content=$(sed -n "${line}p" "$file")
    # Skip rustdoc (/// or //!) and module-level doc-comments entirely.
    if echo "$content" | grep -qE '^\s*(///|//!)'; then
        continue
    fi
    if echo "$content" | grep -qE '^\s*//\s*(pub )?(fn|struct|enum|impl|use crate::|let mut) '; then
        if ! echo "$content" | grep -qE 'MOVING TO|REPLACED BY|MOVING WITHIN CRATE|Example|doc|SAFETY|TODO|NOTE'; then
            echo "AC1.10 violation: commented-out code at $file:$line"
            echo "    > $content"
            fail=1
        fi
    fi
done < <(grep -rnE '^\s*//\s*(pub )?(fn|struct|enum|impl|use crate::|let mut) ' "${workspace_dirs[@]}" || true)

if [ "$fail" -eq 0 ]; then
    echo "audit: clean (AC1.7–AC1.10)"
fi
exit "$fail"
