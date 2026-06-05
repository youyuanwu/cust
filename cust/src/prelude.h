/* cust prelude.
 *
 * Auto-materialised by `cust build` into target/<profile>/prelude.h
 * and force-included into every translation unit via `-include`.
 *
 * The C23 attribute syntax `[[cust::name]]` is accepted by clang
 * but has no plugin handling yet — the macro spellings below are
 * the form that actually does something today. The macros expand
 * to both a real clang attribute (so the contract has teeth even
 * without the plugin) and an `annotate(...)` attribute (so the
 * plugin can recognise the decl after macro expansion).
 */

#ifndef CUST_PRELUDE_H
#define CUST_PRELUDE_H

/* cust_pub: export this decl from the crate. Use on **functions
 * and variables**, where visibility actually applies.
 *   - visibility("default") lifts the symbol over -fvisibility=hidden.
 *   - annotate("cust::pub") lets the plugin spot it and emit a
 *     forward declaration into the per-module fragment header
 *     (target/<profile>/.h-fragments/<crate>/<mod>.cust.h).
 */
#define cust_pub               __attribute__((visibility("default"), annotate("cust::pub")))

/* cust_pub_t: export a **type declaration** (typedef, struct, union,
 * enum) from the crate. Same plugin behaviour as `cust_pub`, but
 * skips the visibility attribute — types have no linkage, so
 * applying `visibility("default")` to a typedef produces a clang
 * `'visibility' attribute ignored [-Wignored-attributes]` warning.
 * Use this for any `cust_pub_t typedef X y;` / `cust_pub_t struct
 * X { ... };` site. Plugin treats both forms identically.
 */
#define cust_pub_t             __attribute__((annotate("cust::pub")))

#define cust_pub_crate         __attribute__((annotate("cust::pub_crate")))
#define cust_must_use          __attribute__((warn_unused_result))
#define cust_deprecated(msg)   __attribute__((deprecated(msg)))
#define cust_unused            __attribute__((unused))
#define cust_noreturn          _Noreturn

/* Reserved for later plugin work: */
/*   cust_cfg, cust_feature, cust_derive, cust_no_panic */

/* cust_test / cust_test_ignore: mark a unit-test function. v0.3.2
 * discovers these via a driver pre-pass scanner (see
 * docs/design/v0.3.2.md V32D-2); plugin v1 in v0.4 joins as a
 * second discovery backend behind the same `__cust_tests[]`
 * contract.
 *
 * In the test variant build (`cust test`, which injects
 * `-DCUST_TEST_BUILD=1`) the macros expand to an annotate-only
 * attribute so the plugin can spot them after macro expansion.
 * In a normal `cust build` the macros expand to
 * `__attribute__((unused)) static`, which keeps tests
 * type-checked but excludes them from the public surface and
 * from the resulting archive.
 *
 * Pre-pass scanner restriction: the marker, return type
 * (`int` or `void`), and function name must all appear on the
 * same source line. Cargo-style `cust_test\n  int foo(void)`
 * is rejected by the v0.3.2 regex. Plugin v1 lifts this.
 */
#ifdef CUST_TEST_BUILD
#  define cust_test            __attribute__((annotate("cust::test")))
#  define cust_test_ignore     __attribute__((annotate("cust::test_ignore")))
#else
#  define cust_test            __attribute__((unused)) static
#  define cust_test_ignore     __attribute__((unused)) static
#endif

/* cust_panic / cust_assert family: assertion macros usable from
 * inside `cust_test` functions. They expand to no-ops outside a
 * test build, so the prelude carries no link-time dependency on
 * `cust_panic_impl` in normal builds. The runner TU (emitted by
 * the driver as `target/<profile>/test/<crate>/cust_test_main.c`)
 * defines `cust_panic_impl` to write `<msg>` + `at <file>:<line>`
 * to stderr and call `_exit(101)` — Rust's `cargo test` exit code
 * for an assertion failure.
 */
#ifdef CUST_TEST_BUILD
_Noreturn void cust_panic_impl(const char *file, int line, const char *msg);
#  define cust_panic(msg)         cust_panic_impl(__FILE__, __LINE__, (msg))
#  define cust_assert(e)          ((e) ? (void)0 : cust_panic("assertion failed: " #e))
#  define cust_assert_eq(a, b)                                              \
        ((a) == (b) ? (void)0                                               \
                    : cust_panic("assertion failed: `(" #a ") == (" #b ")`"))
#  define cust_assert_ne(a, b)                                              \
        ((a) != (b) ? (void)0                                               \
                    : cust_panic("assertion failed: `(" #a ") != (" #b ")`"))
#else
#  define cust_panic(msg)         ((void)(msg))
#  define cust_assert(e)          ((void)0)
#  define cust_assert_eq(a, b)    ((void)(sizeof((a) == (b))))
#  define cust_assert_ne(a, b)    ((void)(sizeof((a) != (b))))
#endif

/* cust_main: bin-crate entry point. Aliased to `main` so the user
 * can write `int cust_main(void) { ... }` and the C runtime sees
 * `main`. This is a plain alias today; a future cust runtime
 * (panic install, signal handling, argv normalisation) may grow
 * a real `main` that calls `cust_main` from inside.
 */
#define cust_main              main

#endif /* CUST_PRELUDE_H */
