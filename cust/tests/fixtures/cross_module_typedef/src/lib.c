/* Regression fixture for the surface-pass typedef bug.
 *
 * `types` exports `[[cust::pub]] typedef __SIZE_TYPE__ cmt_usize;`.
 * `mem` defines a `[[cust::pub]]` function whose return type
 * is the imported typedef. Before the fix, the surface pass
 * blanked the `#cust use crate::types;` line, clang's implicit-
 * int recovery rendered `cmt_usize` as `int`, and the generated
 * `cross_module_typedef.h` published a return type one *register
 * width* too small on x86-64. The driver test asserts the
 * typedef name (or, as a weaker fallback, `unsigned long`) wins.
 */

#cust mod types;
#cust mod mem;
