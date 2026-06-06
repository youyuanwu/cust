// v0.4.0 slice C fixture — sidecar emission (V40D-6, RQ-V40-2).
//
// Verifies the TSV format the plugin writes for [[cust::test]]
// and [[cust::test_ignore]] decls. Driven by run_sidecar_test.sh
// which compiles with -DCUST_TEST_BUILD=1 and asserts the
// sidecar bytes match expectations line-by-line.

[[cust::test]] int test_alpha(void) {
    return 0;
}

// Void-returning test variant (V40D-14 covers both signatures).
[[cust::test]] void test_beta(void) {
    /* fallthrough — success implied. */
}

// Ignored test variant.
[[cust::test_ignore]] int test_gamma_ignored(void) {
    return 0;
}
