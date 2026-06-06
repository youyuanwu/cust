// v0.4.0 slice A fixture — verifies the [[cust::pub]] ParsedAttrInfo
// recogniser fires for both function and type decls, and that
// plain / crate / repr modifiers all reach the AnnotateAttr path.
//
// The runner (run_recognition_test.sh) compiles with
// -fsyntax-only -Wall -Werror, so any -Wunknown-attributes
// warning here is a hard failure. Identifiers prefixed with
// `pub_func_` or `pub_type_` are extracted and grepped for in
// the emitted fragment header.

// Plain [[cust::pub]] on a function — should appear in fragment
// header AND get visibility("default") in the symbol table (we
// don't check the symbol table here; that's a slice E cwork
// integration concern).
[[cust::pub]] int pub_func_plain(int x);

// [[cust::pub]] on a typedef — should appear in fragment header.
// Visibility lift is skipped (decl-kind-aware behaviour). The
// -Werror in the runner catches any -Wignored-attributes
// regression.
[[cust::pub]] typedef int pub_type_plain;

// [[cust::pub(crate)]] — should appear in fragment header with
// the pub_crate payload (slice A only checks the identifier
// surfaces; the pub-vs-pub_crate concat-step filtering is
// slice D driver work).
[[cust::pub(crate)]] int pub_func_crate_only(void);

// [[cust::pub(repr)]] on a struct — slice A only checks the
// identifier surfaces; full body export is slice B. The C23
// attribute placement rule for struct declarations is
// `struct [[…]] tag`, not `[[…]] struct tag`.
struct [[cust::pub(repr)]] pub_type_repr_struct {
    int x;
    int y;
};
