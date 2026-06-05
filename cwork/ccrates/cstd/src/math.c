/* cstd::math — integer primitives.
 *
 * All operations are total and branch-free where it costs nothing.
 * Naming is `<op>_i32` so the same module can grow `_i64` / `_u32`
 * companions without colliding.
 *
 * Signatures use cstd's own `i32` alias (declared in the sibling
 * `types` module), not `int32_t` — the generated `cstd.h` should
 * not depend on `<stdint.h>`. The underlying type is whatever
 * clang resolves `__INT32_TYPE__` to (`int` on every
 * cust-supported target).
 */

#cust use crate::types;

cust_pub i32 cstd_min_i32(i32 a, i32 b) {
    return a < b ? a : b;
}

cust_pub i32 cstd_max_i32(i32 a, i32 b) {
    return a > b ? a : b;
}

cust_pub i32 cstd_abs_i32(i32 x) {
    /* Avoid the I32_MIN UB hazard of `-x` by masking. */
    i32 mask = x >> 31;
    return (x + mask) ^ mask;
}

cust_pub i32 cstd_clamp_i32(i32 x, i32 lo, i32 hi) {
    if (x < lo) return lo;
    if (x > hi) return hi;
    return x;
}
