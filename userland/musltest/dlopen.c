/* musl-dlopen: dlopen/dlsym/dlclose of libplugin.so. */
#include <dlfcn.h>
#include <stdio.h>

int main(void) {
    void *h = dlopen("libplugin.so", RTLD_NOW);
    if (!h) { printf("dlopen failed: %s\n", dlerror()); return 1; }
    int (*add)(int, int) = (int (*)(int, int))dlsym(h, "plugin_add");
    int (*calls)(void) = (int (*)(void))dlsym(h, "plugin_calls");
    const char **name = dlsym(h, "plugin_name");
    if (!add || !calls || !name) { printf("dlsym failed: %s\n", dlerror()); return 1; }
    int r = add(40, 2);
    add(1, 1);
    printf("dlopen OK: %s says %d after %d calls\n", *name, r, calls());
    if (dlsym(h, "no_such_symbol") == 0) printf("dlerror: %s\n", dlerror());
    return dlclose(h) != 0 || r != 42 || calls() != 2;
}
