/* cust prelude.
 *
 * Auto-materialised by `cust build` into target/<profile>/prelude.h
 * and force-included into every translation unit via `-include`.
 *
 * v0.4.0 (V40D-7) retired the decl-annotation macros (`cust_pub`,
 * `cust_pub_t`, `cust_pub_crate`, `cust_test`, `cust_test_ignore`).
 * Decl annotation is now C23 attribute spelling only —
 * `[[cust::pub]]`, `[[cust::pub_crate]]`, `[[cust::pub_repr]]`,
 * `[[cust::test]]`, `[[cust::test_ignore]]` — recognised by the
 * plugin's `ParsedAttrInfo` registrars. The plugin handles the
 * decl-kind-aware visibility lift, the non-test-build internal
 * linkage attachment (V40D-14), and every other behaviour the
 * old macros encoded.
 *
 * What stays in this file: function-like macros that need
 * use-site expansion (sourceloc capture, conditional eval,
 * control-flow injection). The plugin literally cannot replace
 * these — they're textbook macro work.
 */

#ifndef CUST_PRELUDE_H
#define CUST_PRELUDE_H

/* Convenience aliases that don't carry any cust-specific
 * semantics — these are stable identifiers that map to whatever
 * clang attribute (or built-in) is the natural spelling today.
 * No plugin involvement, no cross-version magic. */
#define cust_must_use          __attribute__((warn_unused_result))
#define cust_deprecated(msg)   __attribute__((deprecated(msg)))
#define cust_unused            __attribute__((unused))
#define cust_noreturn          _Noreturn

/* cust_panic / cust_assert family: assertion macros usable from
 * inside `[[cust::test]]` functions. They expand to no-ops
 * outside a test build, so the prelude carries no link-time
 * dependency on `cust_panic_impl` in normal builds. The runner
 * TU (emitted by the driver as
 * `target/<profile>/test/<crate>/cust_test_main.c`) defines
 * `cust_panic_impl` to write `<msg>` + `at <file>:<line>` to
 * stderr and call `_exit(101)` — Rust's `cargo test` exit code
 * for an assertion failure.
 *
 * In non-test builds the assertion expansions still type-check
 * their operands via `sizeof((a) op (b))` so cust_test bodies
 * can't bitrot silently.
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
