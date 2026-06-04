# cust clang plugin (v0).
#
# v0.2 scaffold per docs/design/v0.2.md V2D-2. Standalone CMake
# project — never invoked by `cargo build`. Built via
# `cargo run -p plugin-build` (which shells out to `cmake -B build
# && cmake --build build`).
#
# The driver discovers the resulting `libcust_plugin.so` via:
#   1. $CUST_PLUGIN env var (absolute path)
#   2. target/<profile>/libcust_plugin.so
#
# Today the plugin is a stub that registers a no-op
# PluginASTAction so we can prove the end-to-end loading path
# with `clang -fplugin=<path>`. Fragment-header synthesis lands
# in a follow-up commit.

cmake_minimum_required(VERSION 3.20)
project(cust_plugin LANGUAGES CXX)

set(CMAKE_CXX_STANDARD 20)
set(CMAKE_CXX_STANDARD_REQUIRED ON)
set(CMAKE_CXX_EXTENSIONS OFF)
set(CMAKE_POSITION_INDEPENDENT_CODE ON)

# Pin to whatever clang the user pointed CMake at via
# CMAKE_PREFIX_PATH (set by plugin-build/) or fall back to the
# system install (/usr/lib/llvm-<MAJOR>).
find_package(Clang REQUIRED CONFIG)

message(STATUS "cust_plugin: using Clang ${CLANG_VERSION_MAJOR} at ${CLANG_INSTALL_PREFIX}")

add_library(cust_plugin MODULE
    src/plugin.cc
)

target_include_directories(cust_plugin SYSTEM PRIVATE
    ${CLANG_INCLUDE_DIRS}
    ${LLVM_INCLUDE_DIRS}
)

target_compile_definitions(cust_plugin PRIVATE ${LLVM_DEFINITIONS})

# Plugins must NOT link against LLVM/Clang libs — the host clang
# process already provides every symbol. Linking them in would
# duplicate static state (cl::opt registries, etc.) and crash.
# We do need to allow undefined symbols at link time, though.
set_target_properties(cust_plugin PROPERTIES
    PREFIX "lib"
    SUFFIX ".so"
)
if(NOT CMAKE_HOST_SYSTEM_NAME STREQUAL "Darwin")
    target_link_options(cust_plugin PRIVATE
        # Allow unresolved symbols (provided by host clang at load
        # time). On macOS this would be `-undefined dynamic_lookup`
        # but v0.2 is Linux-only per v0.2.md.
        "-Wl,--allow-shlib-undefined"
    )
endif()

# Match clang's expected C++ ABI flags — clang itself is built
# with -fno-rtti and -fno-exceptions; plugins that disagree
# crash at load time on dynamic_cast of internal AST nodes.
target_compile_options(cust_plugin PRIVATE
    -fno-rtti
    -fno-exceptions
    -fvisibility=hidden
    -Wall
    -Wextra
)
