/* with_itests — fixture for cust v0.4.3 integration tests.
 *
 * Public surface: `add` / `mul`. A crate-private `secret`
 * helper (unannotated `static`, internal linkage) is NOT part
 * of the published `<crate>.h`, so integration tests under
 * tests/ cannot reach it (V43D-3 boundary test lives in cli.rs).
 *
 * One unit test lives here in src/ so the integration-test
 * banner can be checked alongside the unit-test banner. */

[[cust::pub]] int add(int a, int b) { return a + b; }
[[cust::pub]] int mul(int a, int b) { return a * b; }

static int secret(void) { return 7; }

[[cust::test]] int test_secret_is_seven(void) {
    cust_assert_eq(secret(), 7);
    return 0;
}
