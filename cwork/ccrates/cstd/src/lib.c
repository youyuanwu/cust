/* cstd — foundational shared crate under `cwork/ccrates/`.
 *
 * Mirrors (in spirit) Rust's `core`: a small set of obvious building
 * blocks that don't pull in anything ambient.
 *
 * Layout:
 *   src/lib.c    — crate root; declares submodules + cstd_version()
 *   src/types.c  — Rust-aligned primitive aliases
 *                  (i8/.../i64, u8/.../u64, usize, isize, f32, f64)
 *   src/math.c   — integer min/max/abs/clamp over `i32`
 *   src/mem.c    — strlen / memcmp wrappers, returning `usize`/`i32`
 *   src/geom.c   — `[[cust::pub_repr]] struct point` + distance
 *                  (v0.4.0 dogfood for the plugin v1 body-export
 *                  path; consumers can construct `point` by value)
 *   src/alloc.c  — `cstd_alloc` allocator handle (vtable + state)
 *                  plus the libc-backed `cstd_alloc_system()`;
 *                  Rust-`Allocator`-shaped foundation for every
 *                  owning type in the crate
 *   src/string.c — `cstd_str` borrowed view + `cstd_string` owned
 *                  growable string; takes a `cstd_alloc` by value
 *                  in its `_in` constructors
 *   src/vec.c    — `cstd_vec` owned growable type-erased dynamic
 *                  array (runtime `elem_size`/`elem_align`); same
 *                  allocator-by-value + fallible-grow contract as
 *                  `cstd_string`
 *   src/rc.c     — `struct cstd_rc`: kref-shaped single-threaded
 *                  refcount (`init` / `get` / `put(release_fn)`);
 *                  consumers embed it intrusively and recover the
 *                  owning struct in their `release` with offsetof
 *                  arithmetic
 *   src/arc.c    — `struct cstd_arc`: atomic sibling of `cstd::rc`
 *                  for shared-across-threads refcounting; same
 *                  `init` / `get` / `put` shape, `Arc<T>`-style
 *                  memory orderings (Relaxed get, Release dec +
 *                  Acquire fence on the drop path)
 *   src/list.c   — `struct cstd_list_head`: intrusive doubly-
 *                  linked list (kernel `list_head` shape). Purely
 *                  structural — owns no storage, allocates nothing,
 *                  never touches the allocator. An element can sit
 *                  in multiple lists by holding multiple link
 *                  fields.
 *
 * ─── conventions ────────────────────────────────────────
 *
 * Error reporting:
 *   - Functions with binary outcomes return `bool` (`true` = ok,
 *     `false` = failed). The vast majority of cstd's fallible
 *     surface is allocator-driven OOM / size-arithmetic overflow,
 *     which collapse to one recovery and so warrant no enum.
 *   - When a module grows a function with ≥3 distinguishable
 *     outcomes, it declares its OWN error enum, named
 *     `cstd_<mod>_err` with `CSTD_<MOD>_OK = 0` and `CSTD_<MOD>_E_*`
 *     variants. Each module ships its own `cstd_<mod>_err_str()`
 *     if a human-readable form is needed.
 *   - Errors NEVER cross module boundaries. A vec that calls into
 *     the allocator does not propagate an alloc-layer enum; it
 *     maps the failure to its own `CSTD_VEC_E_*` variant at the
 *     call site. This keeps each enum actually-meaningful and
 *     prevents the global-error-enum kitchen-sink failure mode.
 *   - Coexistence over migration: introducing an enum on a module
 *     does NOT retire its existing `bool`-returning functions.
 *     Add an `_e`-suffixed sibling only when both shapes are
 *     genuinely wanted.
 *
 * Ownership / drop:
 *   - Every owning type stores its `cstd_alloc` by value so its
 *     `_free` function takes only the value, not a separate
 *     allocator argument.
 *   - `_free` is idempotent on zero-initialised and
 *     already-freed values; this is what makes
 *     `[[gnu::cleanup(cstd_<t>_free)]]` safe on any control-flow
 *     path.
 *
 * Macros:
 *   - cstd defines NO macros. Use-site ergonomics that need
 *     preprocessor work (e.g. `cust_cleanup(fn)`) belong in the
 *     driver-owned prelude, not in any crate.
 *
 * Downstream usage from another crate in the same workspace:
 *
 *     [dependencies]
 *     cstd = { path = "../cstd" }
 *
 *     // src/lib.c
 *     #cust use cstd;
 *
 *     [[cust::pub]] i32 my_max(i32 a, i32 b) {
 *         return cstd_max_i32(a, b);
 *     }
 */

#cust mod types;
#cust mod math;
#cust mod mem;
#cust mod geom;
#cust mod alloc;
#cust mod string;
#cust mod vec;
#cust mod rc;
#cust mod arc;
#cust mod list;

#cust use crate::types;

/* The cust major/minor this crate was authored against. Bumps with
 * the driver. Useful for downstream `static_assert`s once we expose
 * a real version macro. */
[[cust::pub]] u32 cstd_version(void) {
    return (0u << 16) | (4u << 8) | 3u; /* 0.4.3 */
}
