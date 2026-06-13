/* Package-named bin: multibin. */
#include <stdio.h>

#cust use multibin;

[[cust::pub]] int cust_main(void) {
    printf("main: answer = %d\n", multibin_answer());
    return 0;
}
