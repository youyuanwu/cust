#include <stdio.h>

int main(int argc, char **argv) {
    /* exit code = argc, so the test can assert how many argv arrived */
    for (int i = 1; i < argc; i++) {
        printf("argv[%d]=%s\n", i, argv[i]);
    }
    return argc;
}
