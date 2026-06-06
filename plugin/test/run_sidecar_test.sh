#!/usr/bin/env bash
# Plugin sidecar test runner (v0.4.0 slice C).
#
# Compiles the fixture with -DCUST_TEST_BUILD=1 +
# `-fplugin-arg-cust-test-sidecar-out=...` + `-module=test_module`,
# then asserts:
#   * The sidecar file exists.
#   * Each expected (qname, fn_kind, ignored) triple appears on
#     its own line in the sidecar.
#   * Line count matches the number of expected triples.
#
# Args:
#   $1 = path to clang
#   $2 = path to libcust_plugin.so
#   $3 = path to fixture .c file
#   $4 = sidecar output path
#   $5,$6,$7 = first expected triple   (qname, fn_kind, ignored)
#   $8,$9,$10 = second expected triple
#   ...
# Expected triples are passed as flat args; the runner groups
# them in 3s. This sidesteps CMake `add_test` not honouring
# bash ANSI-C `$'...\t...'` quoting.

set -euo pipefail

CLANG="$1"
PLUGIN="$2"
FIXTURE="$3"
SIDECAR="$4"
shift 4

if (( $# % 3 != 0 )); then
    echo "FAIL: expected triples must come in groups of 3 (got $#)" >&2
    exit 1
fi

rm -f "$SIDECAR"

"$CLANG" \
    -fsyntax-only \
    -std=c23 \
    -Wall -Werror \
    -DCUST_TEST_BUILD=1 \
    -fplugin="$PLUGIN" \
    -fplugin-arg-cust-test-sidecar-out="$SIDECAR" \
    -fplugin-arg-cust-module=test_module \
    "$FIXTURE"

if [[ ! -f "$SIDECAR" ]]; then
    echo "FAIL: plugin produced no sidecar at $SIDECAR" >&2
    exit 1
fi

# Group the trailing args into (qname, fn_kind, ignored) triples
# and check each one is present.
TAB=$'\t'
missing=()
expected_count=0
while (( $# > 0 )); do
    qname="$1"; fn_kind="$2"; ignored="$3"
    shift 3
    expected_count=$((expected_count + 1))
    needle="${qname}${TAB}${fn_kind}${TAB}${ignored}${TAB}"
    if ! grep -Fq "$needle" "$SIDECAR"; then
        missing+=("${qname}|${fn_kind}|${ignored}")
    fi
done

if (( ${#missing[@]} > 0 )); then
    echo "FAIL: sidecar missing expected entries: ${missing[*]}" >&2
    echo "--- sidecar bytes ---" >&2
    cat "$SIDECAR" >&2
    exit 1
fi

actual_lines=$(wc -l < "$SIDECAR")
if (( actual_lines != expected_count )); then
    echo "FAIL: sidecar has $actual_lines lines, expected $expected_count" >&2
    cat "$SIDECAR" >&2
    exit 1
fi

# Verify the file column is the fixture's absolute path and the
# line column parses as a positive integer.
fixture_abs=$(readlink -f "$FIXTURE")
while IFS=$'\t' read -r _qname _kind _ignored file line; do
    if [[ "$file" != "$fixture_abs" ]]; then
        echo "FAIL: file column $file != fixture path $fixture_abs" >&2
        exit 1
    fi
    if ! [[ "$line" =~ ^[0-9]+$ ]] || (( line < 1 )); then
        echo "FAIL: line column $line is not a positive integer" >&2
        exit 1
    fi
done < "$SIDECAR"

echo "OK"
