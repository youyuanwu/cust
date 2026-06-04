#include <stdint.h>

cust_pub int32_t hello_add(int32_t a, int32_t b) {
    return a + b;
}

static int32_t internal_helper(int32_t x) {
    return x * 2;
}

cust_pub int32_t hello_double(int32_t x) {
    return internal_helper(x);
}
