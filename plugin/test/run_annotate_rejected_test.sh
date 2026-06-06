#!/usr/bin/env bash
# Plugin annotate-rejection test runner (v0.4.0 slice E, V40D-7).
#
# Args:
#   $1 = path to clang
#   $2 = path to libcust_plugin.so
#   $3 = path to fixture .c file
#   $4 = scratch fragment-out path
#
# Asserts:
#   * `real_c23_pub` appears in the fragment header (genuine
#     C23 attribute, recognised).
#   * `legacy_annotated_pub` does NOT appear (user-written
#     `annotate("cust::pub")` is ignored without the sentinel
#     marker the plugin's ParsedAttrInfo recognisers attach).

set -euo pipefail

CLANG="$1"
PLUGIN="$2"
FIXTURE="$3"
FRAGMENT="$4"

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

if ! grep -q "real_c23_pub" "$FRAGMENT"; then
    echo "FAIL: real_c23_pub missing from fragment (recogniser regression)" >&2
    cat "$FRAGMENT" >&2
    exit 1
fi

if grep -q "legacy_annotated_pub" "$FRAGMENT"; then
    echo "FAIL: legacy_annotated_pub leaked into fragment (V40D-7 sentinel-marker enforcement regression)" >&2
    cat "$FRAGMENT" >&2
    exit 1
fi

echo "OK"
