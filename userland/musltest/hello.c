/* musl-hello: stdio, malloc, environment and time through musl. */
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>
#include <unistd.h>

int main(int argc, char **argv) {
    char *buf = malloc(64);
    snprintf(buf, 64, "hello from musl %s", argc > 1 ? argv[1] : "world");
    struct timespec ts;
    clock_gettime(CLOCK_MONOTONIC, &ts);
    printf("%s (pid %d, HOME=%s, uptime %lds)\n", buf, (int)getpid(), getenv("HOME"), (long)ts.tv_sec);
    free(buf);
    return 0;
}
