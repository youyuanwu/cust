// v0.4.0 slice C fixture — V40D-14 test signature error cases.
//
// Each [[cust::test]] decl below has a bad signature. The plugin
// must error on each one with wording that mentions the
// identifier so users can locate the offending decl.

[[cust::test]] int test_takes_args(int x) {
    return x;
}

[[cust::test]] long test_wrong_return(void) {
    return 0;
}

[[cust::test]] char *test_pointer_return(void) {
    return 0;
}
