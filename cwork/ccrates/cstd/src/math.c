/* cstd::math — integer primitives.
 *
 * All operations are total and branch-free where it costs nothing.
 * Naming is `<op>_<type>` so the same module can grow `_i64` / `_u32`
 * companions without colliding.
 */

#include <stdint.h>

cust_pub int32_t cstd_min_i32(int32_t a, int32_t b) {
    return a < b ? a : b;
}

cust_pub int32_t cstd_max_i32(int32_t a, int32_t b) {
    return a > b ? a : b;
}

cust_pub int32_t cstd_abs_i32(int32_t x) {
    /* Avoid the INT32_MIN UB hazard of `-x` by masking. */
    int32_t mask = x >> 31;
    return (x + mask) ^ mask;
}

cust_pub int32_t cstd_clamp_i32(int32_t x, int32_t lo, int32_t hi) {
    if (x < lo) return lo;
    if (x > hi) return hi;
    return x;
}
