// v0.4.0 slice B fixture — V40D-4 error cases.
//
// Each of these should produce a clang error of the form:
//   "cannot export body of `<name>`: `[[cust::pub_repr]]` is only
//    meaningful on struct, union, or enum decls (this is <kind>)
//      hint: drop `pub_repr` and use `[[cust::pub]]`"
//
// The runner (run_repr_errors_test.sh) compiles with
// -fsyntax-only and asserts exit-nonzero + that all three of
// the affected identifier names appear in stderr (so the order
// of error emission doesn't matter).

[[cust::pub_repr]] int repr_on_function(void);

[[cust::pub_repr]] int repr_on_variable;

[[cust::pub_repr]] typedef int repr_on_typedef;
