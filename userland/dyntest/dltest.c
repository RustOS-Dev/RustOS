/* dltest: dlopen/dlsym/dlerror through ld-rustos. */
#include "sys.h"

extern void *dlopen(const char *name, int flags);
extern void *dlsym(void *handle, const char *name);
extern int dlclose(void *handle);
extern char *dlerror(void);

__attribute__((force_align_arg_pointer)) void _start(void) {
    void *h = dlopen("libgreet.so", 2);
    if (!h) { puts_fd(2, dlerror()); puts_fd(2, "\n"); sys_exit(1); }
    int (*greet)(const char *) = (int (*)(const char *))dlsym(h, "greet");
    const char **version = (const char **)dlsym(h, "greet_version");
    if (!greet || !version) { puts_fd(2, "dlsym failed\n"); sys_exit(1); }
    greet("dltest");
    /* A library with TLS loaded at run time. */
    void *t = dlopen("libtlsdemo.so", 2);
    int (*bump)(void) = t ? (int (*)(void))dlsym(t, "tls_bump") : 0;
    int *counter = t ? (int *)dlsym(t, "tls_counter") : 0;
    int v = bump ? bump() : -1;
    if (!dlopen("libnothere.so", 2)) {
        puts_fd(1, "dlerror: ");
        puts_fd(1, dlerror());
        puts_fd(1, "\n");
    }
    if (v == 6 && counter && *counter == 6 && dlclose(h) == 0) {
        puts_fd(1, "dlopen OK (");
        puts_fd(1, *version);
        puts_fd(1, ")\n");
        sys_exit(0);
    }
    puts_fd(2, "dlopen FAILED\n");
    sys_exit(1);
}
