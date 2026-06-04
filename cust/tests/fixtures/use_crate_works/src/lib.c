#include <stdint.h>

#cust mod util;
#cust use crate::util;

cust_pub int32_t use_crate_works_total(void) {
    return use_crate_works_util_get();
}
