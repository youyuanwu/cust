/* cstd — foundational shared crate under `cwork/ccrates/`.
 *
 * Mirrors (in spirit) Rust's `core`: a small set of obvious building
 * blocks that don't pull in anything ambient. Today the surface is
 * deliberately tiny — every export is a plain function over
 * primitives because cross-crate type sharing (`[[cust::pub(repr)]]`)
 * is a v0.4 plugin feature.
 *
 * Layout:
 *   src/lib.c    — crate root; declares submodules + a couple of
 *                  top-level re-exports
 *   src/math.c   — integer min/max/abs/clamp
 *   src/mem.c    — strlen / memcmp wrappers with cust visibility
 *
 * Downstream usage from another crate in the same workspace:
 *
 *     [dependencies]
 *     cstd = { path = "../cstd" }
 *
 *     // src/lib.c
 *     #cust use cstd;
 *
 *     cust_pub int32_t my_max(int32_t a, int32_t b) {
 *         return cstd_max_i32(a, b);
 *     }
 */

#cust mod math;
#cust mod mem;

#include <stdint.h>

/* The cust major/minor this crate was authored against. Bumps with
 * the driver. Useful for downstream `static_assert`s once we expose
 * a real version macro. */
cust_pub uint32_t cstd_version(void) {
    return (0u << 16) | (3u << 8) | 0u; /* 0.3.0 */
}
