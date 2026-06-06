#!/usr/bin/env bash
# Plugin internal-linkage test runner (v0.4.0 slice C, V40D-14).
#
# Compiles the slice-C sidecar fixture WITHOUT -DCUST_TEST_BUILD,
# producing a real object file. Asserts that none of the test_*
# functions appear as external symbols (`nm` upper-case T):
# instead they should be local (`t`) per the InternalLinkageAttr
# the plugin attaches when CUST_TEST_BUILD is undefined.
#
# Args:
#   $1 = path to clang
#   $2 = path to libcust_plugin.so
#   $3 = path to fixture .c file
#   $4 = object output path
#   $5 ... = expected test function names

set -euo pipefail

CLANG="$1"
PLUGIN="$2"
FIXTURE="$3"
OBJECT="$4"
shift 4

rm -f "$OBJECT"

# Note: no -DCUST_TEST_BUILD, no -fplugin-arg-cust-test-sidecar-out.
# The plugin still recognises the attributes (visibility-equivalent
# at the AST level) but attaches InternalLinkageAttr + UnusedAttr.
# We need -fvisibility=hidden + the fragment-out arg to mirror
# what cust build does in production; without -fvisibility=hidden
# the V40D-14 internal-linkage marker is the ONLY thing keeping
# the symbols out, which is exactly what V40D-14 is supposed to
# guarantee.
"$CLANG" \
    -c \
    -std=c23 \
    -Wall -Werror \
    -fvisibility=hidden \
    -fplugin="$PLUGIN" \
    -o "$OBJECT" \
    "$FIXTURE"

if [[ ! -f "$OBJECT" ]]; then
    echo "FAIL: clang produced no object at $OBJECT" >&2
    exit 1
fi

# Each expected name must appear ONLY as a local symbol (`t`) and
# NEVER as a global symbol (`T`). `nm` letter case encodes
# binding: lowercase = local, uppercase = external.
for name in "$@"; do
    if nm "$OBJECT" | grep -qE " T $name\$"; then
        echo "FAIL: $name leaked as external symbol (V40D-14 violation)" >&2
        nm "$OBJECT" | grep " $name\$" >&2 || true
        exit 1
    fi
    # Should still exist as a local symbol — if it's missing
    # entirely, UnusedAttr + dead-code elimination dropped it
    # (acceptable but worth a heads-up).
    if ! nm "$OBJECT" | grep -qE " [tTbBdDrR] $name\$"; then
        echo "warn: $name not present in symbol table (probably dead-code eliminated)" >&2
    fi
done

echo "OK"
