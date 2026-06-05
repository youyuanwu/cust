/* @generated runner template — cust v0.3.2 test harness
 *
 * Embedded into cust/src/test_runner.rs via include_str!.
 * Concatenated ahead of per-test `extern` decls + the
 * `__cust_tests[]` table + `int main(...)` (see render_main_c).
 *
 * V32D-6 / V32D-7 in docs/design/v0.3.2.md:
 *   - per-test fork isolation
 *   - exit code 101 on assertion failure (matches cargo test)
 *   - --list short-circuits; no flag parsing beyond filter +
 *     --list (V32D-9 / V32D-10 / RQ-V32-1)
 *
 * Linux-only. fork / waitpid / WIFEXITED / WIFSIGNALED / _exit.
 */

#include <errno.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/types.h>
#include <sys/wait.h>
#include <unistd.h>

/* Definition for the cust_panic_impl forward-declared by the
 * prelude when -DCUST_TEST_BUILD=1 is set. Writes the assertion
 * message + source location to stderr, then exits 101 — the same
 * exit code cargo test uses for an assertion failure. */
_Noreturn void cust_panic_impl(const char *file, int line, const char *msg) {
    fprintf(stderr, "%s\n  at %s:%d\n", msg, file, line);
    fflush(stderr);
    _exit(101);
}

enum cust_test_fn_kind { CUST_TEST_FN_VOID, CUST_TEST_FN_INT };

struct cust_test_entry {
    const char            *qname;
    void                  *fn_ptr;
    enum cust_test_fn_kind fn_kind;
    int                    ignored;
    const char            *file;
    int                    line;
};

/* Run one test in a forked subprocess. Returns 1 on pass, 0 on
 * fail. A "pass" is: child exited 0; the int-returning kind
 * returned 0 explicitly, the void-returning kind ran to
 * completion. A "fail" is: child exited non-zero, or was killed
 * by a signal, or fork itself failed. */
static int cust_test_run_one(const struct cust_test_entry *e) {
    pid_t pid = fork();
    if (pid < 0) {
        fprintf(stderr, "fork failed: %s\n", strerror(errno));
        return 0;
    }
    if (pid == 0) {
        /* child */
        if (e->fn_kind == CUST_TEST_FN_INT) {
            int (*f)(void) = (int (*)(void))e->fn_ptr;
            int rc = f();
            _exit(rc == 0 ? 0 : 1);
        } else {
            void (*f)(void) = (void (*)(void))e->fn_ptr;
            f();
            _exit(0);
        }
    }
    int status = 0;
    if (waitpid(pid, &status, 0) < 0) {
        fprintf(stderr, "waitpid failed: %s\n", strerror(errno));
        return 0;
    }
    if (WIFEXITED(status) && WEXITSTATUS(status) == 0) {
        return 1;
    }
    return 0;
}

/* Driver. Returns the process exit code:
 *   0 if every forked test passed (and no other error)
 *   1 if any test failed
 * --list short-circuits with code 0.
 *
 * Parses a single positional <filter> and the flag --list from
 * argv. Everything else is ignored (forward compatibility for
 * v0.4 additions like --nocapture / --test-threads / --exact). */
int cust_test_run(int argc, char **argv,
                  const struct cust_test_entry *tests, int n_tests) {
    const char *filter = NULL;
    int list_mode = 0;
    for (int i = 1; i < argc; i++) {
        if (strcmp(argv[i], "--list") == 0) {
            list_mode = 1;
        } else if (filter == NULL && argv[i][0] != '-') {
            filter = argv[i];
        }
    }

    if (list_mode) {
        int count = 0;
        for (int i = 0; i < n_tests; i++) {
            if (filter && !strstr(tests[i].qname, filter)) {
                continue;
            }
            printf("%s: test\n", tests[i].qname);
            count++;
        }
        printf("\n%d tests, 0 benchmarks\n", count);
        return 0;
    }

    /* Pre-count so the "running N tests" line is accurate (Cargo
     * parity: counts forked + ignored, not filtered_out). */
    int matched = 0;
    int filtered_out = 0;
    for (int i = 0; i < n_tests; i++) {
        if (filter && !strstr(tests[i].qname, filter)) {
            filtered_out++;
            continue;
        }
        matched++;
    }
    printf("\nrunning %d tests\n", matched);

    int passed = 0, failed = 0, ignored = 0;
    for (int i = 0; i < n_tests; i++) {
        if (filter && !strstr(tests[i].qname, filter)) {
            continue;
        }
        if (tests[i].ignored) {
            printf("test %s ... ignored\n", tests[i].qname);
            ignored++;
            continue;
        }
        if (cust_test_run_one(&tests[i])) {
            printf("test %s ... ok\n", tests[i].qname);
            passed++;
        } else {
            printf("test %s ... FAILED\n", tests[i].qname);
            failed++;
        }
    }

    const char *result = (failed == 0) ? "ok" : "FAILED";
    if (filtered_out > 0) {
        printf("\ntest result: %s. %d passed; %d failed; %d ignored; %d filtered out\n",
               result, passed, failed, ignored, filtered_out);
    } else {
        printf("\ntest result: %s. %d passed; %d failed; %d ignored\n",
               result, passed, failed, ignored);
    }
    return (failed == 0) ? 0 : 1;
}

/* `int main(int argc, char **argv)` is appended by render_main_c
 * after the per-test extern decls + __cust_tests[] table. */
