/* with_tests — fixture for v0.3.2 slice C end-to-end tests.
 * Library has a couple of public functions, a couple of cust_test
 * functions exercising them, one cust_test_ignore-marked test
 * that would fail if run, and one void-returning test. */

cust_pub int add(int a, int b) { return a + b; }
cust_pub int mul(int a, int b) { return a * b; }

cust_test int test_add_basic(void) {
    cust_assert_eq(add(2, 3), 5);
    cust_assert_eq(add(-1, 1), 0);
    return 0;
}

cust_test void test_mul_void_kind(void) {
    cust_assert(mul(3, 4) == 12);
}

cust_test_ignore int test_skipped(void) {
    cust_assert(0);  /* would fail if not ignored */
    return 0;
}
