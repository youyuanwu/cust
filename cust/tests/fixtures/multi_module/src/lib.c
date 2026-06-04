#include <stdint.h>

#cust mod util;
#cust mod parser;

extern int32_t multi_module_util_get(void);
extern int32_t multi_module_parser_count(void);

cust_pub int32_t multi_module_total(void) {
    return multi_module_util_get() + multi_module_parser_count();
}
