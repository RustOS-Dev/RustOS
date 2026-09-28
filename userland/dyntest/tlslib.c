/* libtlsdemo.so: thread-local variables in a shared library. */
__thread int tls_counter = 5;
__thread char tls_buf[32];
int tls_bump(void) {
    tls_buf[0] = 'x';
    return ++tls_counter;
}
