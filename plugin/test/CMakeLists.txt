# cust clang plugin tests (v0.4.0 slice A scaffold).
#
# Each test is a shell-driven invocation of `clang -fplugin=...`
# against a fixture .c file, asserting that the plugin produces
# the expected fragment header / diagnostics. Shell-driven (not a
# C++ LibTooling test binary) because:
#
#  1. It exercises the actual end-to-end load path users hit.
#  2. The infrastructure cost is one ~30-line bash script;
#     LibTooling needs CMake-time linking against clang libs and
#     a separate test main per scenario.
#  3. Slices B / C will extend the same pattern with more
#     fixtures (golden fragment headers, phase-isolation, sidecar
#     emission).
#
# Tests can be run with `ctest --test-dir plugin/build` after
# `cargo run -p plugin-build`.

# Locate clang at configure time. CMake's CMAKE_CXX_COMPILER is
# clang++ (set by plugin-build); the matching clang lives next to
# it.
get_filename_component(_clang_dir "${CMAKE_CXX_COMPILER}" DIRECTORY)
find_program(CLANG_C_EXECUTABLE
    NAMES clang
    HINTS "${_clang_dir}"
    REQUIRED
)
message(STATUS "cust_plugin tests: clang = ${CLANG_C_EXECUTABLE}")

# Helper macro: registers one fixture-driven test.
function(cust_plugin_add_recognition_test name)
    add_test(
        NAME ${name}
        COMMAND
            "${CMAKE_CURRENT_SOURCE_DIR}/run_recognition_test.sh"
            "${CLANG_C_EXECUTABLE}"
            "$<TARGET_FILE:cust_plugin>"
            "${CMAKE_CURRENT_SOURCE_DIR}/fixtures/${name}.c"
            "${CMAKE_CURRENT_BINARY_DIR}/${name}.cust.h"
    )
endfunction()

# Goldenfile variant (V40D-8): emits the fragment header and
# diffs it against a checked-in golden. Used for slice B's
# pub_repr body coverage where the *exact bytes* matter, not
# just identifier presence.
function(cust_plugin_add_golden_test name)
    add_test(
        NAME ${name}
        COMMAND
            "${CMAKE_CURRENT_SOURCE_DIR}/run_golden_test.sh"
            "${CLANG_C_EXECUTABLE}"
            "$<TARGET_FILE:cust_plugin>"
            "${CMAKE_CURRENT_SOURCE_DIR}/fixtures/${name}.c"
            "${CMAKE_CURRENT_BINARY_DIR}/${name}.cust.h"
            "${CMAKE_CURRENT_SOURCE_DIR}/goldens/${name}.cust.h"
    )
endfunction()

cust_plugin_add_recognition_test(test_c23_pub_recognition)
cust_plugin_add_golden_test(test_pub_repr_body)

# V40D-7 slice E sentinel-marker enforcement: user-written
# `annotate("cust::pub")` is silently ignored; only decls that
# went through the plugin's ParsedAttrInfo recognisers (which
# attach the sentinel) are picked up.
add_test(
    NAME test_annotate_rejected
    COMMAND
        "${CMAKE_CURRENT_SOURCE_DIR}/run_annotate_rejected_test.sh"
        "${CLANG_C_EXECUTABLE}"
        "$<TARGET_FILE:cust_plugin>"
        "${CMAKE_CURRENT_SOURCE_DIR}/fixtures/test_annotate_rejected.c"
        "${CMAKE_CURRENT_BINARY_DIR}/test_annotate_rejected.cust.h"
)

# V40D-4 error cases (slice B): pub_repr on function / variable /
# typedef must each produce the V40D-4 error wording.
add_test(
    NAME test_pub_repr_errors
    COMMAND
        "${CMAKE_CURRENT_SOURCE_DIR}/run_repr_errors_test.sh"
        "${CLANG_C_EXECUTABLE}"
        "$<TARGET_FILE:cust_plugin>"
        "${CMAKE_CURRENT_SOURCE_DIR}/fixtures/test_pub_repr_errors.c"
        "repr_on_function"
        "repr_on_variable"
        "repr_on_typedef"
)

# Slice C — V40D-6 + RQ-V40-2 sidecar emission.
# Expected entries are passed as flat (qname, fn_kind, ignored)
# triples; the runner reconstructs the tab-delimited needle.
# This sidesteps CMake `add_test` not honouring bash ANSI-C
# `$'...\t...'` quoting.
add_test(
    NAME test_test_sidecar
    COMMAND
        "${CMAKE_CURRENT_SOURCE_DIR}/run_sidecar_test.sh"
        "${CLANG_C_EXECUTABLE}"
        "$<TARGET_FILE:cust_plugin>"
        "${CMAKE_CURRENT_SOURCE_DIR}/fixtures/test_test_sidecar.c"
        "${CMAKE_CURRENT_BINARY_DIR}/test_test_sidecar.tsv"
        "test_module::test_alpha"          "int"  "0"
        "test_module::test_beta"           "void" "0"
        "test_module::test_gamma_ignored"  "int"  "1"
)

# Slice C — V40D-14 non-test-build linkage attachment. The fixture
# uses [[cust::test]] decls; without -DCUST_TEST_BUILD the plugin
# attaches InternalLinkageAttr + UnusedAttr so the symbols don't
# leak into the regular artifact.
add_test(
    NAME test_test_internal_linkage
    COMMAND
        "${CMAKE_CURRENT_SOURCE_DIR}/run_internal_linkage_test.sh"
        "${CLANG_C_EXECUTABLE}"
        "$<TARGET_FILE:cust_plugin>"
        "${CMAKE_CURRENT_SOURCE_DIR}/fixtures/test_test_sidecar.c"
        "${CMAKE_CURRENT_BINARY_DIR}/test_test_internal_linkage.o"
        "test_alpha"
        "test_beta"
        "test_gamma_ignored"
)

# Slice C — V40D-5 phase isolation.
add_test(
    NAME test_phase_isolation
    COMMAND
        "${CMAKE_CURRENT_SOURCE_DIR}/run_phase_isolation_test.sh"
        "${CLANG_C_EXECUTABLE}"
        "$<TARGET_FILE:cust_plugin>"
        "${CMAKE_CURRENT_SOURCE_DIR}/fixtures/test_phase_isolation.c"
        "${CMAKE_CURRENT_BINARY_DIR}/test_phase_isolation.cust.h"
        "${CMAKE_CURRENT_BINARY_DIR}/test_phase_isolation.o"
)

# Slice C — V40D-14 signature validation. [[cust::test]] functions
# must take `void` and return `int` or `void`.
add_test(
    NAME test_test_signature_errors
    COMMAND
        "${CMAKE_CURRENT_SOURCE_DIR}/run_test_errors_test.sh"
        "${CLANG_C_EXECUTABLE}"
        "$<TARGET_FILE:cust_plugin>"
        "${CMAKE_CURRENT_SOURCE_DIR}/fixtures/test_test_signature_errors.c"
        "must (return|take)"
        "test_takes_args"
        "test_wrong_return"
        "test_pointer_return"
)
