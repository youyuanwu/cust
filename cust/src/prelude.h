/* cust prelude.
 *
 * Auto-materialised by `cust build` into target/<profile>/prelude.h
 * and force-included into every translation unit via `-include`.
 *
 * The C23 attribute syntax `[[cust::name]]` is accepted by clang
 * but has no plugin handling yet — the macro spellings below are
 * the form that actually does something today. The macros expand
 * to both a real clang attribute (so the contract has teeth even
 * without the plugin) and an `annotate(...)` attribute (so the
 * plugin can recognise the decl after macro expansion).
 */

#ifndef CUST_PRELUDE_H
#define CUST_PRELUDE_H

/* cust_pub: export this decl from the crate.
 *   - visibility("default") lifts the symbol over -fvisibility=hidden.
 *   - annotate("cust::pub") lets the plugin spot it and emit a
 *     forward declaration into the per-module fragment header
 *     (target/<profile>/.h-fragments/<crate>/<mod>.cust.h).
 */
#define cust_pub               __attribute__((visibility("default"), annotate("cust::pub")))
#define cust_pub_crate         __attribute__((annotate("cust::pub_crate")))
#define cust_must_use          __attribute__((warn_unused_result))
#define cust_deprecated(msg)   __attribute__((deprecated(msg)))
#define cust_unused            __attribute__((unused))
#define cust_noreturn          _Noreturn

/* Reserved for later plugin work: */
/*   cust_test, cust_cfg, cust_feature, cust_derive, cust_no_panic */

#endif /* CUST_PRELUDE_H */
