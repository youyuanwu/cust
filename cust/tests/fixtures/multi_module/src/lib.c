#include <stdint.h>

#cust mod util;
#cust mod parser;

#cust use crate::util;
#cust use crate::parser;

cust_pub int32_t multi_module_total(void) {
    return multi_module_util_get() + multi_module_parser_count();
}
