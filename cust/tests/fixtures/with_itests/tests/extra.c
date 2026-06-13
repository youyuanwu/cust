/* Second integration file — separate exe (V43D-5). */
#cust use with_itests;

[[cust::test]] int test_add_again(void) {
    cust_assert_eq(add(10, 20), 30);
    return 0;
}
