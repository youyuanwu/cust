// v0.4.0 slice A+B fixture — verifies that the five-name model
// ParsedAttrInfo recognisers fire for both function and type decls,
// across plain pub / pub_crate / pub_repr. Per V40D-7 the design uses
// three separate attribute names (not one parameterised `pub(crate)`
// form) because clang's expression-parser silently drops identifier
// args from C++/C23 attributes.
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

// [[cust::pub_crate]] on a function — slice B treats pub_crate
// as None for fragment emission (slice D adds the concat-step
// filter with /*c*/ prefix per V40D-3). So `crate_only_helper`
// should NOT appear in the fragment. Its inclusion in this
// fixture is to verify the recogniser parses + attaches without
// erroring.
//
// Identifier is named with no `pub_func_` prefix so the runner's
// "every pub_func_*/pub_type_* identifier appears in fragment"
// assertion doesn't flag it.
[[cust::pub_crate]] int crate_only_helper(void);

// [[cust::pub_repr]] on a struct — slice B emits the full body
// (V40D-4). The runner checks the identifier appears; the
// per-body byte verification lives in the goldenfile test
// (test_pub_repr_body, slice B). C23 attribute placement for
// struct declarations is `struct [[…]] tag`, not pre-struct.
struct [[cust::pub_repr]] pub_type_repr_struct {
    int x;
    int y;
};

// Plain [[cust::pub]] on an enum — emits `enum <name>;` forward
// declaration per V40D-4 sub-case. C23 fixed underlying type
// keeps the forward decl portable. C23 attribute placement for
// enum is `enum [[…]] tag : type`, not pre-enum (same rule as
// struct).
enum [[cust::pub]] pub_type_plain_enum : int { ALPHA, BETA };
