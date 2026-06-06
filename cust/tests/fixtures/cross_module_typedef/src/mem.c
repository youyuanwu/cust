#cust use crate::types;

/* Public function whose ABI depends on cross-module typedef
 * resolution during the surface pass. Returns a constant just
 * so the function actually compiles. */
[[cust::pub]] cmt_usize cmt_mem_size(void) {
    return (cmt_usize)42;
}
