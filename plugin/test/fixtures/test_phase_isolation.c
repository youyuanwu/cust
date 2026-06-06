// v0.4.0 slice C fixture — V40D-5 phase isolation.
//
// Used by run_phase_isolation_test.sh. The fixture itself is
// just a single [[cust::pub]] decl to give the plugin something
// to attach to; the test's actual verification is "phase 2
// invocation with fragment-out arg errors out, phase 2 without
// it succeeds."

[[cust::pub]] int phase_iso_func(void);
