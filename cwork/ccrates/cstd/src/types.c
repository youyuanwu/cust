/* cstd::types — Rust-aligned primitive type aliases.
 *
 * Defines `i8`/`i16`/`i32`/`i64`/`u8`/`u16`/`u32`/`u64`/`usize`/
 * `isize`/`f32`/`f64` as `[[cust::pub]] typedef`s so consumers
 * reach them by `#cust use cstd;` rather than by `#include
 * <stdint.h>` themselves. This is the Cargo-parity move: any
 * type a consumer uses must be reachable through the producing
 * crate's surface.
 *
 * Implementation note: we use clang's `__INT32_TYPE__` /
 * `__UINT64_TYPE__` / `__SIZE_TYPE__` builtin macros (each
 * expands to the underlying primitive — `int`, `unsigned long`,
 * `unsigned long`, etc.) so cstd itself doesn't have to
 * `#include <stdint.h>`. Clang's pretty-printer resolves these
 * to the concrete primitive when emitting the typedef into the
 * fragment header, so the generated cstd.h contains
 *
 *     typedef int           i32;
 *     typedef unsigned long u64;
 *     typedef unsigned long usize;
 *
 * with no `#include` leakage.
 *
 * `bool` is intentionally NOT defined here: it is a C23
 * language keyword (and a `<stdbool.h>` typedef in C99/C11/C17).
 * Rust's `bool` is likewise a keyword, not a type alias, so
 * there is no parity to preserve. Consumers spell it `bool`
 * directly.
 */

[[cust::pub]] typedef __INT8_TYPE__   i8;
[[cust::pub]] typedef __INT16_TYPE__  i16;
[[cust::pub]] typedef __INT32_TYPE__  i32;
[[cust::pub]] typedef __INT64_TYPE__  i64;

[[cust::pub]] typedef __UINT8_TYPE__  u8;
[[cust::pub]] typedef __UINT16_TYPE__ u16;
[[cust::pub]] typedef __UINT32_TYPE__ u32;
[[cust::pub]] typedef __UINT64_TYPE__ u64;

/* Pointer-sized integers. `usize` is the unsigned address-width
 * type (Rust's `usize` / C's `size_t` / `uintptr_t`); `isize` is
 * the signed counterpart (Rust's `isize` / C's `ssize_t` /
 * `intptr_t`). */
[[cust::pub]] typedef __SIZE_TYPE__   usize;
[[cust::pub]] typedef __INTPTR_TYPE__ isize;

/* IEEE-754 binary32 and binary64. clang has no `__FLOAT_TYPE__`
 * builtin (floats aren't size-parameterised in the same way), so
 * these are spelled directly. Every cust-supported platform has
 * `float` = binary32 and `double` = binary64. */
[[cust::pub]] typedef float           f32;
[[cust::pub]] typedef double          f64;
