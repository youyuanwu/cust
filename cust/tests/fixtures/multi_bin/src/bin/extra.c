/* Extra bin auto-discovered from src/bin/extra.c → named `extra`. */
#include <stdio.h>

#cust use multibin;

[[cust::pub]] int cust_main(void) {
    printf("extra: answer = %d\n", multibin_answer());
    return 0;
}
