/* cstd — foundational shared crate under `cwork/ccrates/`.
 *
 * Mirrors (in spirit) Rust's `core`: a small set of obvious building
 * blocks that don't pull in anything ambient. Today the surface is
 * deliberately tiny — every export is a plain function over
 * primitives because cross-crate type sharing (`[[cust::pub(repr)]]`)
 * is a v0.4 plugin feature.
 *
 * Layout:
 *   src/lib.c    — crate root; declares submodules + cstd_version()
 *   src/types.c  — Rust-aligned primitive aliases
 *                  (i8/.../i64, u8/.../u64, usize, isize, f32, f64)
 *   src/math.c   — integer min/max/abs/clamp over `i32`
 *   src/mem.c    — strlen / memcmp wrappers, returning `usize`/`i32`
 *
 * Downstream usage from another crate in the same workspace:
 *
 *     [dependencies]
 *     cstd = { path = "../cstd" }
 *
 *     // src/lib.c
 *     #cust use cstd;
 *
 *     cust_pub i32 my_max(i32 a, i32 b) {
 *         return cstd_max_i32(a, b);
 *     }
 */

#cust mod types;
#cust mod math;
#cust mod mem;

#cust use crate::types;

/* The cust major/minor this crate was authored against. Bumps with
 * the driver. Useful for downstream `static_assert`s once we expose
 * a real version macro. */
cust_pub u32 cstd_version(void) {
    return (0u << 16) | (3u << 8) | 1u; /* 0.3.1 */
}
