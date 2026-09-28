/* tlstest: TLS in the program (local-exec) and in a library (initial-exec
 * from the program, general-dynamic inside the library). */
#include "sys.h"

extern __thread int tls_counter;
extern __thread char tls_buf[32];
extern int tls_bump(void);
static __thread long own = 40;
static __thread int zeroed;

__attribute__((force_align_arg_pointer)) void _start(void) {
    int a = tls_bump();          /* 6, via __tls_get_addr in the library */
    tls_counter += 10;           /* 16, via TPOFF64 in the program */
    int b = tls_bump();          /* 17 */
    own += 2;
    if (a == 6 && b == 17 && tls_buf[0] == 'x' && own == 42 && zeroed == 0) {
        puts_fd(1, "TLS OK\n");
        sys_exit(0);
    }
    puts_fd(2, "TLS FAILED\n");
    sys_exit(1);
}
