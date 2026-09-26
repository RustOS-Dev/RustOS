/* Minimal system-call helpers for the libc-free dynamic linking tests. */
static inline long sys3(long n, long a, long b, long c) {
    long r;
    __asm__ volatile("syscall" : "=a"(r) : "a"(n), "D"(a), "S"(b), "d"(c) : "rcx", "r11", "memory");
    return r;
}
static inline unsigned long slen(const char *s) {
    unsigned long n = 0;
    while (s[n]) n++;
    return n;
}
static inline void puts_fd(int fd, const char *s) { sys3(1, fd, (long)s, (long)slen(s)); }
static inline void sys_exit(int code) { sys3(60, code, 0, 0); for (;;) {} }
