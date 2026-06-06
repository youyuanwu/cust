/* Pointer-width unsigned integer alias, exported with no
 * `<stddef.h>` include of its own — clang's `__SIZE_TYPE__`
 * builtin resolves to whatever the underlying primitive is
 * (`unsigned long` on x86-64 Linux). */
[[cust::pub]] typedef __SIZE_TYPE__ cmt_usize;
