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

cust_plugin_add_recognition_test(test_c23_pub_recognition)
