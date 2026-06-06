#!/usr/bin/env bash
# Plugin recognition-test runner (v0.4.0 slice A).
#
# Args:
#   $1 = path to clang
#   $2 = path to libcust_plugin.so
#   $3 = path to fixture .c file
#   $4 = path where the plugin should write its fragment header
#
# Asserts:
#   * clang exits 0 (the plugin recognises [[cust::pub]] without
#     -Wunknown-attributes errors or parse failures).
#   * The fragment header file is produced at $4.
#   * The fragment header lists every function/typedef the
#     fixture marks as pub (one line per `pub_func_*` / `pub_type_*`
#     identifier).
#
# Each fixture .c file uses the bare names `pub_func_*` and
# `pub_type_*` so the assertion is independent of how the
# pretty-printer formats the surrounding signature.

set -euo pipefail

CLANG="$1"
PLUGIN="$2"
FIXTURE="$3"
FRAGMENT="$4"

# Fresh slate — stamping invariant means a left-over file from a
# previous run would silently turn this test into a no-op.
rm -f "$FRAGMENT"

# Run clang. -fsyntax-only matches the V40D-5 phase-1 contract;
# the plugin emits the fragment header during HandleTranslationUnit.
# -Wall -Werror catches accidental -Wunknown-attributes regressions.
"$CLANG" \
    -fsyntax-only \
    -std=c23 \
    -Wall -Werror \
    -fplugin="$PLUGIN" \
    -fplugin-arg-cust-fragment-out="$FRAGMENT" \
    "$FIXTURE"

# Fragment header must exist.
if [[ ! -f "$FRAGMENT" ]]; then
    echo "FAIL: plugin produced no fragment header at $FRAGMENT" >&2
    exit 1
fi

# Every `pub_func_*` / `pub_type_*` identifier in the fixture must
# appear in the fragment.
missing=()
while IFS= read -r ident; do
    if ! grep -q "$ident" "$FRAGMENT"; then
        missing+=("$ident")
    fi
done < <(grep -oE '\bpub_(func|type)_[a-z_0-9]+' "$FIXTURE" | sort -u)

if (( ${#missing[@]} > 0 )); then
    echo "FAIL: fragment header missing identifiers: ${missing[*]}" >&2
    echo "--- fragment header ---" >&2
    cat "$FRAGMENT" >&2
    exit 1
fi

echo "OK"
