/* libdl.so: link-time stub; ld-rustos provides the real functions. */
void *dlopen(const char *name, int flags) { (void)name; (void)flags; return 0; }
void *dlsym(void *handle, const char *name) { (void)handle; (void)name; return 0; }
int dlclose(void *handle) { (void)handle; return -1; }
char *dlerror(void) { return "dynamic linker stub"; }
