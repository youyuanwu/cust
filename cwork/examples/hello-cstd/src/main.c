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
 *   cstd version = 0x000405 (0.4.5)
 *   distance_sq((0,0), (3,4)) = 25
 */

#include <stdio.h>

#cust use cstd;

[[cust::pub]] int cust_main(void) {
    i32 a = 3, b = 7;
    printf("max(%d, %d) = %d\n", a, b, cstd_max_i32(a, b));

    const char *greeting = "hello, cstd";
    /* cstd_strlen returns `usize`, which is `unsigned long` per
     * the resolved __SIZE_TYPE__ macro — %zu is the C-portable
     * format specifier for that width. */
    printf("strlen(\"%s\") = %zu\n",
           greeting,
           (unsigned long)cstd_strlen(greeting));

    /* v0.4.0 dogfood: construct a cstd_point by value (only
     * possible because [[cust::pub_repr]] exports the body)
     * and pass it through cstd_point_distance_sq. */
    struct cstd_point origin = {0, 0};
    struct cstd_point target = {3, 4};
    printf("distance_sq((%d,%d), (%d,%d)) = %d\n",
           origin.x, origin.y, target.x, target.y,
           cstd_point_distance_sq(origin, target));

    printf("cstd version = 0x%06x (0.4.5)\n", cstd_version());
    return 0;
}
