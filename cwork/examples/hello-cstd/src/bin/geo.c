/* geo — a second hello-cstd binary (v0.4.4 multi-bin dogfood,
 * V44D-10). Auto-discovered from `src/bin/geo.c`, so its target
 * name is `geo` and it builds to `target/<profile>/geo`. It
 * consumes cstd's public surface differently from `main.c` —
 * exercising the [[cust::pub_repr]] `cstd_point` body and
 * `cstd_point_distance_sq` over a small table of points.
 *
 * Run with:
 *
 *   cd cwork
 *   ../target/debug/cust run -p hello-cstd --bin geo
 *
 * (`cust run -p hello-cstd` without `--bin` is now ambiguous —
 * hello-cstd has two bins, `geo` and `hello-cstd`.)
 */

#include <stdio.h>

#cust use cstd;

[[cust::pub]] int cust_main(void) {
    struct cstd_point pts[] = {
        {0, 0},
        {3, 4},
        {6, 8},
    };
    const int n = (int)(sizeof(pts) / sizeof(pts[0]));

    /* Print the squared distance from the origin for each point. */
    for (int i = 0; i < n; i++) {
        printf("geo: |(%d,%d)|^2 = %d\n",
               pts[i].x, pts[i].y,
               cstd_point_distance_sq(pts[0], pts[i]));
    }
    return 0;
}
