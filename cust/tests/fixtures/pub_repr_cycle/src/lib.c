/* pub_repr_cycle — synthetic `[[cust::pub_repr]]` import cycle.
 *
 * v0.4.5 V45D-6 fixture: modules `a` and `b` mutually
 * `#cust use crate::…` each other, so the import graph has a
 * 2-node strongly-connected component. The emitter cannot express
 * that as a fine-grained per-module DAG (a `DEPENDS` cycle is a
 * hard CMake error), so it falls back to a single coarse
 * `internal surface-cycle` command that surfaces both modules
 * together via the fixed-point loop.
 *
 * Each side exports a `[[cust::pub_repr]]` struct holding a
 * pointer to the other side's struct — a pointer to an incomplete
 * type is legal C, so the cycle converges and the crate compiles.
 */
#cust mod a;
#cust mod b;
