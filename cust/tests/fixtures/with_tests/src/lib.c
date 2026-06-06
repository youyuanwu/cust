/* with_tests — fixture for cust test end-to-end tests.
 * Library has a couple of public functions, a couple of
 * [[cust::test]] functions exercising them, one
 * [[cust::test_ignore]]-marked test that would fail if run,
 * and one void-returning test. */

[[cust::pub]] int add(int a, int b) { return a + b; }
[[cust::pub]] int mul(int a, int b) { return a * b; }

[[cust::test]] int test_add_basic(void) {
    cust_assert_eq(add(2, 3), 5);
    cust_assert_eq(add(-1, 1), 0);
    return 0;
}

[[cust::test]] void test_mul_void_kind(void) {
    cust_assert(mul(3, 4) == 12);
}

[[cust::test_ignore]] int test_skipped(void) {
    cust_assert(0);  /* would fail if not ignored */
    return 0;
}
