/* libplugin.so: loaded with dlopen by musl-dlopen. */
static __thread int calls;   /* dynamic TLS in a dlopen'ed library */
int plugin_add(int a, int b) { calls++; return a + b; }
int plugin_calls(void) { return calls; }
const char *plugin_name = "plugin 1.0";
