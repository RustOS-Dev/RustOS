/* dyntest: a dynamically linked program (built both as PIE and non-PIE). */
#include "sys.h"

extern int greet(const char *who);
extern int greet_count;
extern const char *greet_version;

__attribute__((force_align_arg_pointer)) void _start(void) {
    int n = greet("dyntest");
    if (n == 101 && greet_count == 101 && greet_version[0] == 'l') {
        puts_fd(1, "dynamic linking OK (");
        puts_fd(1, greet_version);
        puts_fd(1, ")\n");
        sys_exit(0);
    }
    puts_fd(2, "dynamic linking FAILED\n");
    sys_exit(1);
}
