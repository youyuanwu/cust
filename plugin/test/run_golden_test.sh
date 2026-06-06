#!/usr/bin/env bash
# Plugin goldenfile test runner (v0.4.0 slice B).
#
# V40D-8 enforces that re-running the plugin on the same TU
# produces byte-identical fragment header output (the stamping
# invariant in §4 is what avoids cascade rebuilds when an
# importee's surface didn't change).
#
# This runner spawns clang -fplugin=… over a fixture, compares
# the emitted fragment header bytes against a checked-in golden,
# and prints a unified diff on mismatch. Update the golden
# whenever the fixture or pretty-printer changes — but
# deliberately: the diff is the design's signal that downstream
# consumers will rebuild.
#
# Args:
#   $1 = path to clang
#   $2 = path to libcust_plugin.so
#   $3 = path to fixture .c file
#   $4 = path where the plugin should write its fragment header
#   $5 = path to the golden file

set -euo pipefail

CLANG="$1"
PLUGIN="$2"
FIXTURE="$3"
FRAGMENT="$4"
GOLDEN="$5"

rm -f "$FRAGMENT"

"$CLANG" \
    -fsyntax-only \
    -std=c23 \
    -Wall -Werror \
    -fplugin="$PLUGIN" \
    -fplugin-arg-cust-fragment-out="$FRAGMENT" \
    "$FIXTURE"

if [[ ! -f "$FRAGMENT" ]]; then
    echo "FAIL: plugin produced no fragment header at $FRAGMENT" >&2
    exit 1
fi

if ! diff -u "$GOLDEN" "$FRAGMENT"; then
    echo "FAIL: fragment header bytes differ from golden ($GOLDEN)" >&2
    echo "      to accept the new output, replace the golden file" >&2
    echo "      with: cp $FRAGMENT $GOLDEN" >&2
    exit 1
fi

echo "OK"
