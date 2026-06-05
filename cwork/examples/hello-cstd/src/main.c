/* hello-cstd — the first end-to-end example consuming the
 * cwork/ccrates family. Demonstrates:
 *
 *   - workspace path-dep across the cwork/ccrates / cwork/examples
 *     subdirs (resolved via the cwork workspace's [workspace] members
 *     list)
 *   - `#cust use cstd;` lowering to an include of cstd's generated
 *     public header — which now exports its own Rust-aligned
 *     primitive typedefs (`i32`, `usize`, ...), so this file
 *     needs zero `<stdint.h>` / `<stddef.h>` chasing
 *   - the V31D-1 bin auto-inference (no [[bin]] table; src/main.c
 *     alone is enough)
 *   - linking the bin against cstd's libcstd.a via the v0.3.1
 *     --start-group / --end-group archive wrap
 *
 * Run with:
 *
 *   cd cwork
 *   ../target/debug/cust run -p hello-cstd
 *
 * Expected output:
 *
 *   max(3, 7) = 7
 *   strlen("hello, cstd") = 11
 *   cstd version = 0x000301 (0.3.1)
 */

#include <stdio.h>

#cust use cstd;

cust_pub int cust_main(void) {
    i32 a = 3, b = 7;
    printf("max(%d, %d) = %d\n", a, b, cstd_max_i32(a, b));

    const char *greeting = "hello, cstd";
    printf("strlen(\"%s\") = %zu\n", greeting, cstd_strlen(greeting));

    printf("cstd version = 0x%06x (0.3.1)\n", cstd_version());
    return 0;
}
