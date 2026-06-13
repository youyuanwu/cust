// cstd integration tests — allocator + container growth under
// pressure (V43D-7). Exercises the public `cstd_string` / `cstd_vec`
// APIs against the system allocator, pushing past the initial
// capacity to force reallocation paths.
//
// V43D-2 is deferred, so the small helper below is inlined per file
// rather than shared from tests/common/.
#cust use cstd;

// Inlined helper: a fresh system-allocator-backed string.
static struct cstd_string fresh_string(void) {
    return cstd_string_new_in(cstd_alloc_system());
}

[[cust::test]] int test_string_grow_past_capacity(void) {
    struct cstd_string s = fresh_string();
    // Push enough bytes to force at least one regrow.
    for (int i = 0; i < 1000; i++) {
        cust_assert(cstd_string_push_byte(&s, (u8)('a' + (i % 26))));
    }
    cust_assert_eq((i32)cstd_string_len(&s), 1000);
    cust_assert(cstd_string_capacity(&s) >= 1000);
    cstd_string_free(&s);
    return 0;
}

[[cust::test]] int test_string_push_str_roundtrip(void) {
    struct cstd_string s = fresh_string();
    cust_assert(cstd_string_push_cstr(&s, "hello, "));
    cust_assert(cstd_string_push_cstr(&s, "world"));
    struct cstd_str view = cstd_string_as_str(&s);
    cust_assert_eq((i32)view.len, 12);
    cust_assert(cstd_str_eq(view, cstd_str_from_cstr("hello, world")));
    cstd_string_free(&s);
    return 0;
}

[[cust::test]] int test_vec_push_many_i32(void) {
    struct cstd_vec v = cstd_vec_new_in(sizeof(i32), _Alignof(i32), cstd_alloc_system());
    for (i32 i = 0; i < 500; i++) {
        cust_assert(cstd_vec_push(&v, &i));
    }
    cust_assert_eq((i32)cstd_vec_len(&v), 500);
    // Spot-check a few stored elements.
    i32 got = 0;
    const void *p = cstd_vec_get_const(&v, 0);
    got = *(const i32 *)p;
    cust_assert_eq(got, 0);
    p = cstd_vec_get_const(&v, 499);
    got = *(const i32 *)p;
    cust_assert_eq(got, 499);
    // Pop the last element back out.
    cust_assert(cstd_vec_pop(&v, &got));
    cust_assert_eq(got, 499);
    cust_assert_eq((i32)cstd_vec_len(&v), 499);
    cstd_vec_free(&v);
    return 0;
}
