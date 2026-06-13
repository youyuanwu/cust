/* Integration test against the public surface only (V43D-3). */
#cust use with_itests;

[[cust::test]] int test_add_via_public(void) {
    cust_assert_eq(add(2, 3), 5);
    cust_assert_eq(add(-1, 1), 0);
    return 0;
}

[[cust::test]] void test_mul_via_public(void) {
    cust_assert(mul(3, 4) == 12);
}
