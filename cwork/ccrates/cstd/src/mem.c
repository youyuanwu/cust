/* cstd::mem — thin wrappers over libc memory/string primitives.
 *
 * Why wrap at all? Two reasons:
 *   1. Visibility: every export carries `cust_pub`, so the symbol
 *      table of the final binary documents exactly which libc
 *      surfaces a downstream crate actually reached for.
 *   2. Stability: when cust grows a freestanding profile (OQ-8) the
 *      `cstd_*` names stay put while the underlying implementation
 *      swaps to a no-libc version.
 *
 * Public signatures use cstd's `usize` / `i32` aliases (declared
 * in the sibling `types` module) so the generated `cstd.h` is
 * `<stddef.h>`-free. The libc wrappers themselves still need
 * `<stddef.h>` / `<string.h>` internally; those includes stay
 * private to this TU.
 */

#cust use crate::types;

#include <stddef.h>
#include <string.h>

cust_pub usize cstd_strlen(const char *s) {
    return strlen(s);
}

cust_pub i32 cstd_memcmp(const void *a, const void *b, usize n) {
    return memcmp(a, b, n);
}
