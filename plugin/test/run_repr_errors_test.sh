#!/usr/bin/env bash
# Plugin error-case test runner (v0.4.0 slice B).
#
# Asserts that compilation FAILS (nonzero exit) and that each of
# the expected identifier names appears in stderr — so any
# ordering or error-recovery quirk between clang versions
# doesn't matter.
#
# Args:
#   $1 = path to clang
#   $2 = path to libcust_plugin.so
#   $3 = path to fixture .c file
#   $4 ... = identifier names expected to appear in stderr

set -uo pipefail

CLANG="$1"
PLUGIN="$2"
FIXTURE="$3"
shift 3

# Capture stderr; we deliberately don't use -Werror here — we
# want the plugin's own custom errors, not clang's warnings.
stderr_log=$(mktemp)
trap 'rm -f "$stderr_log"' EXIT

# We expect this to fail; don't let set -e abort.
set +e
"$CLANG" \
    -fsyntax-only \
    -std=c23 \
    -fplugin="$PLUGIN" \
    -fplugin-arg-cust-fragment-out=/tmp/_unused_fragment.h \
    "$FIXTURE" 2> "$stderr_log"
exit_code=$?
set -e

if (( exit_code == 0 )); then
    echo "FAIL: expected nonzero exit, got 0" >&2
    cat "$stderr_log" >&2
    exit 1
fi

missing=()
for ident in "$@"; do
    if ! grep -q "$ident" "$stderr_log"; then
        missing+=("$ident")
    fi
done

if (( ${#missing[@]} > 0 )); then
    echo "FAIL: stderr missing expected identifiers: ${missing[*]}" >&2
    echo "--- stderr ---" >&2
    cat "$stderr_log" >&2
    exit 1
fi

# Also assert the V40D-4 hint wording surfaces, so accidental
# wording drift trips the test.
if ! grep -q "drop \`pub_repr\` and use \`\[\[cust::pub\]\]\`" "$stderr_log"; then
    echo "FAIL: V40D-4 hint wording missing from stderr" >&2
    echo "--- stderr ---" >&2
    cat "$stderr_log" >&2
    exit 1
fi

echo "OK"
