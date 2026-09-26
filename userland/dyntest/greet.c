/* libgreet.so: a shared library exercising data/function symbols,
 * RELATIVE relocations (pointer in .data) and an ELF constructor. */
#include "sys.h"

int greet_count = 0;
static const char *prefix = "hello from libgreet.so, ";
const char *greet_version = "libgreet 1.0";

__attribute__((constructor)) static void greet_init(void) { greet_count = 100; }

int greet(const char *who) {
    puts_fd(1, prefix);
    puts_fd(1, who);
    puts_fd(1, "\n");
    return ++greet_count;
}
