#!/usr/bin/env bash
# Plugin phase-isolation test runner (v0.4.0 slice C, V40D-5).
#
# Asserts:
#   (1) Phase 2 (codegen, `-c`) WITH -fplugin-arg-cust-fragment-out
#       → plugin hard-errors with V40D-5 wording.
#   (2) Phase 2 (codegen, `-c`) WITHOUT the arg → succeeds.
#   (3) Phase 1 (-fsyntax-only) WITH the arg → succeeds + fragment
#       file appears.
#
# Args:
#   $1 = path to clang
#   $2 = path to libcust_plugin.so
#   $3 = path to fixture .c file
#   $4 = scratch fragment-out path
#   $5 = scratch object output path

set -uo pipefail

CLANG="$1"
PLUGIN="$2"
FIXTURE="$3"
FRAGMENT="$4"
OBJECT="$5"

err() { echo "FAIL: $1" >&2; exit 1; }

# --- (1) Phase 2 + fragment-out: must fail with V40D-5 wording.
rm -f "$FRAGMENT" "$OBJECT"
stderr_log=$(mktemp)
trap 'rm -f "$stderr_log"' EXIT
set +e
"$CLANG" -c -std=c23 \
    -fplugin="$PLUGIN" \
    -fplugin-arg-cust-fragment-out="$FRAGMENT" \
    -o "$OBJECT" "$FIXTURE" 2> "$stderr_log"
exit_code=$?
set -e
if (( exit_code == 0 )); then
    err "phase-2 with fragment-out should have errored, got exit 0"
fi
if ! grep -q "phase-2 invocation must not write fragment headers" "$stderr_log"; then
    echo "stderr:" >&2; cat "$stderr_log" >&2
    err "phase-2 error message missing V40D-5 wording"
fi

# --- (2) Phase 2 without the arg: must succeed.
rm -f "$OBJECT"
"$CLANG" -c -std=c23 \
    -fplugin="$PLUGIN" \
    -o "$OBJECT" "$FIXTURE" 2> "$stderr_log" || {
        cat "$stderr_log" >&2
        err "phase-2 without fragment-out should have succeeded"
    }
[[ -f "$OBJECT" ]] || err "phase-2 without fragment-out produced no object"

# --- (3) Phase 1 with the arg: must succeed + write the fragment.
rm -f "$FRAGMENT"
"$CLANG" -fsyntax-only -std=c23 \
    -fplugin="$PLUGIN" \
    -fplugin-arg-cust-fragment-out="$FRAGMENT" \
    "$FIXTURE" 2> "$stderr_log" || {
        cat "$stderr_log" >&2
        err "phase-1 with fragment-out should have succeeded"
    }
[[ -f "$FRAGMENT" ]] || err "phase-1 with fragment-out produced no fragment"
if ! grep -q "phase_iso_func" "$FRAGMENT"; then
    cat "$FRAGMENT" >&2
    err "phase-1 fragment missing the decl"
fi

echo "OK"
