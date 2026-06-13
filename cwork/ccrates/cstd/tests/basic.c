// cstd integration tests — public-surface smoke coverage (V43D-7).
//
// These exercise cstd exactly the way a downstream consumer would:
// linked against `libcstd.a` and `#include "cstd.h"` only (no
// crate-private modules, no `[[cust::pub_crate]]` reach). If any
// symbol used here isn't `[[cust::pub]]`-exported, this file fails
// to compile — surfacing the surface gap at integration-test time.
#cust use cstd;

[[cust::test]] int test_min_max_roundtrip(void) {
    cust_assert_eq(cstd_max_i32(3, 7), 7);
    cust_assert_eq(cstd_max_i32(7, 3), 7);
    cust_assert_eq(cstd_min_i32(3, 7), 3);
    cust_assert_eq(cstd_min_i32(-1, -2), -2);
    return 0;
}

[[cust::test]] int test_clamp_and_abs(void) {
    cust_assert_eq(cstd_clamp_i32(5, 0, 10), 5);
    cust_assert_eq(cstd_clamp_i32(-3, 0, 10), 0);
    cust_assert_eq(cstd_clamp_i32(42, 0, 10), 10);
    cust_assert_eq(cstd_abs_i32(-7), 7);
    return 0;
}

[[cust::test]] void test_strlen_public(void) {
    cust_assert(cstd_strlen("") == 0);
    cust_assert(cstd_strlen("hello") == 5);
}

[[cust::test]] int test_point_distance_sq(void) {
    struct cstd_point a = { .x = 0, .y = 0 };
    struct cstd_point b = { .x = 3, .y = 4 };
    cust_assert_eq(cstd_point_distance_sq(a, b), 25);
    return 0;
}

[[cust::test]] int test_version_nonzero(void) {
    cust_assert(cstd_version() != 0u);
    return 0;
}
