// v0.4.0 slice E fixture — V40D-7 sentinel-marker enforcement.
//
// Slice E removed user-facing `annotate("cust::*")` source
// recognition: only decls that go through the plugin's
// `ParsedAttrInfo` recognisers (which attach an internal
// sentinel marker alongside the cust payload) are picked up
// by the AST consumer. Verifies that a decl carrying a
// user-written `__attribute__((annotate("cust::pub")))` is
// silently ignored — does NOT appear in the fragment header.

// User-written annotate: pretends to be cust::pub but lacks
// the sentinel marker. Plugin must NOT emit a fragment-header
// entry for `legacy_annotated_pub`.
__attribute__((annotate("cust::pub"))) int legacy_annotated_pub(void);

// Genuine C23 [[cust::pub]]: goes through the recogniser,
// gets the sentinel attached, IS recognised.
[[cust::pub]] int real_c23_pub(void);
