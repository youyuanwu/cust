/* cust v0.1 prelude.
 *
 * Auto-materialised by `cust build` into target/<profile>/prelude.h
 * and force-included into every translation unit via `-include`.
 *
 * In v0.1 the C23 attribute syntax `[[cust::name]]` is accepted by
 * clang but has no plugin to enforce it. The macro spellings below
 * are the form that actually does something today.
 */

#ifndef CUST_PRELUDE_H
#define CUST_PRELUDE_H

#define cust_pub               __attribute__((visibility("default")))
#define cust_pub_crate         /* no-op in v0.1 */
#define cust_must_use          __attribute__((warn_unused_result))
#define cust_deprecated(msg)   __attribute__((deprecated(msg)))
#define cust_unused            __attribute__((unused))
#define cust_noreturn          _Noreturn

/* Reserved for the v0.2 plugin: */
/*   cust_test, cust_cfg, cust_feature, cust_derive, cust_no_panic */

#endif /* CUST_PRELUDE_H */
