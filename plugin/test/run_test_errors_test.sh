#!/usr/bin/env bash
# Plugin error-case test runner — generalised version of the
# slice B run_repr_errors_test.sh (which is kept for backward
# compatibility with the V40D-4 hint-wording assertion).
#
# Asserts that compilation FAILS (nonzero exit) and that:
#   * Every expected identifier name appears in stderr
#     (order-independent across clang versions).
#   * The expected wording (regex) appears in stderr.
#
# Args:
#   $1 = path to clang
#   $2 = path to libcust_plugin.so
#   $3 = path to fixture .c file
#   $4 = expected wording (passed to `grep -E`)
#   $5 ... = identifier names expected to appear in stderr

set -uo pipefail

CLANG="$1"
PLUGIN="$2"
FIXTURE="$3"
WORDING="$4"
shift 4

stderr_log=$(mktemp)
trap 'rm -f "$stderr_log"' EXIT

# We expect failure; -DCUST_TEST_BUILD=1 so [[cust::test]]
# fixtures don't get diverted into V40D-14's internal-linkage
# path (we want the signature checker to run, which only happens
# during sidecar emission).
set +e
"$CLANG" \
    -fsyntax-only \
    -std=c23 \
    -DCUST_TEST_BUILD=1 \
    -fplugin="$PLUGIN" \
    -fplugin-arg-cust-test-sidecar-out=/tmp/_unused_sidecar.tsv \
    -fplugin-arg-cust-module=err_module \
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
    cat "$stderr_log" >&2
    exit 1
fi

if ! grep -qE "$WORDING" "$stderr_log"; then
    echo "FAIL: expected wording /$WORDING/ missing from stderr" >&2
    cat "$stderr_log" >&2
    exit 1
fi

echo "OK"
