/* musl-desktop: the kernel features Wayland compositors, clients and
 * session managers use (M36): fd passing over Unix sockets, memfd with
 * seals, shared mappings across processes, /dev/shm, inotify, pidfds,
 * close_range, copy_file_range and the extra clocks.
 *
 *   musl-desktop          run the checks
 *   musl-desktop uevent N wait up to N seconds for a kernel uevent and
 *                         print it (NETLINK_KOBJECT_UEVENT)
 *   musl-desktop vt       VT switching as a compositor does it: VT_PROCESS
 *                         mode, K_OFF, DRM master dropped on release and
 *                         taken back on acquire (on the console's tty)
 */
#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <linux/kd.h>
#include <linux/netlink.h>
#include <linux/vt.h>
#include <sys/ioctl.h>
#include <poll.h>
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/inotify.h>
#include <sys/mman.h>
#include <sys/socket.h>
#include <sys/stat.h>
#include <sys/syscall.h>
#include <sys/uio.h>
#include <sys/wait.h>
#include <time.h>
#include <unistd.h>

static int passed, failed;
#define CHECK(name, cond) do { if (cond) passed++; else { failed++; printf("FAIL %s (%s:%d, errno %d)\n", name, __FILE__, __LINE__, errno); } } while (0)

#ifndef F_ADD_SEALS
#define F_ADD_SEALS 1033
#define F_GET_SEALS 1034
#define F_SEAL_SEAL 1
#define F_SEAL_SHRINK 2
#define F_SEAL_GROW 4
#define F_SEAL_WRITE 8
#endif
#ifndef MFD_ALLOW_SEALING
#define MFD_CLOEXEC 1
#define MFD_ALLOW_SEALING 2
#endif

static int send_fd(int sock, int fd) {
    char c = 'F';
    struct iovec iov = { &c, 1 };
    union { struct cmsghdr h; char buf[CMSG_SPACE(sizeof(int))]; } u;
    struct msghdr m = { 0 };
    m.msg_iov = &iov;
    m.msg_iovlen = 1;
    m.msg_control = u.buf;
    m.msg_controllen = sizeof u.buf;
    struct cmsghdr *h = CMSG_FIRSTHDR(&m);
    h->cmsg_level = SOL_SOCKET;
    h->cmsg_type = SCM_RIGHTS;
    h->cmsg_len = CMSG_LEN(sizeof(int));
    memcpy(CMSG_DATA(h), &fd, sizeof fd);
    return sendmsg(sock, &m, 0) == 1 ? 0 : -1;
}

static int recv_fd(int sock) {
    char c;
    struct iovec iov = { &c, 1 };
    union { struct cmsghdr h; char buf[CMSG_SPACE(sizeof(int))]; } u;
    struct msghdr m = { 0 };
    m.msg_iov = &iov;
    m.msg_iovlen = 1;
    m.msg_control = u.buf;
    m.msg_controllen = sizeof u.buf;
    if (recvmsg(sock, &m, 0) != 1)
        return -1;
    struct cmsghdr *h = CMSG_FIRSTHDR(&m);
    if (!h || h->cmsg_type != SCM_RIGHTS)
        return -1;
    int fd;
    memcpy(&fd, CMSG_DATA(h), sizeof fd);
    return fd;
}

/* A client hands a compositor a buffer: a sealed memfd over a socket. */
static void memfd_passing(void) {
    int sv[2];
    CHECK("socketpair", socketpair(AF_UNIX, SOCK_STREAM | SOCK_CLOEXEC, 0, sv) == 0);
    int fd = memfd_create("wl_shm", MFD_CLOEXEC | MFD_ALLOW_SEALING);
    CHECK("memfd_create", fd >= 0);
    CHECK("ftruncate", ftruncate(fd, 8192) == 0);
    unsigned *p = mmap(NULL, 8192, PROT_READ | PROT_WRITE, MAP_SHARED, fd, 0);
    CHECK("mmap memfd", p != MAP_FAILED);
    p[0] = 0xc0ffee;
    p[2047] = 0xbeef;
    CHECK("munmap", munmap(p, 8192) == 0);
    CHECK("add seals", fcntl(fd, F_ADD_SEALS, F_SEAL_SHRINK | F_SEAL_GROW | F_SEAL_WRITE) == 0);
    CHECK("get seals", fcntl(fd, F_GET_SEALS) == (F_SEAL_SHRINK | F_SEAL_GROW | F_SEAL_WRITE));
    CHECK("sealed write", write(fd, "x", 1) < 0 && errno == EPERM);
    CHECK("sealed shrink", ftruncate(fd, 4096) < 0 && errno == EPERM);
    CHECK("sealed grow", ftruncate(fd, 16384) < 0 && errno == EPERM);
    CHECK("sealed mmap rw", mmap(NULL, 4096, PROT_READ | PROT_WRITE, MAP_SHARED, fd, 0) == MAP_FAILED && errno == EPERM);

    pid_t pid = fork();
    if (pid == 0) {
        close(sv[0]);
        int got = recv_fd(sv[1]);
        if (got < 0)
            _exit(10);
        struct stat st;
        if (fstat(got, &st) || st.st_size != 8192)
            _exit(11);
        unsigned *q = mmap(NULL, 8192, PROT_READ, MAP_SHARED, got, 0);
        if (q == MAP_FAILED)
            _exit(12);
        if (q[0] != 0xc0ffee || q[2047] != 0xbeef)
            _exit(13);
        if (fcntl(got, F_GET_SEALS) != (F_SEAL_SHRINK | F_SEAL_GROW | F_SEAL_WRITE))
            _exit(14);
        _exit(0);
    }
    close(sv[1]);
    CHECK("send fd", send_fd(sv[0], fd) == 0);
    int st;
    CHECK("child got the buffer", waitpid(pid, &st, 0) == pid && WIFEXITED(st) && WEXITSTATUS(st) == 0);
    close(sv[0]);
    close(fd);

    /* Without MFD_ALLOW_SEALING the seal set is closed. */
    fd = memfd_create("plain", 0);
    CHECK("unsealable", fcntl(fd, F_ADD_SEALS, F_SEAL_WRITE) < 0 && errno == EPERM);
    CHECK("F_SEAL_SEAL default", fcntl(fd, F_GET_SEALS) == F_SEAL_SEAL);
    close(fd);

    /* A writable shared mapping blocks F_SEAL_WRITE. */
    fd = memfd_create("busy", MFD_ALLOW_SEALING);
    ftruncate(fd, 4096);
    void *w = mmap(NULL, 4096, PROT_READ | PROT_WRITE, MAP_SHARED, fd, 0);
    CHECK("busy seal", fcntl(fd, F_ADD_SEALS, F_SEAL_WRITE) < 0 && errno == EBUSY);
    munmap(w, 4096);
    close(fd);
}

/* Writes in one process show in another's shared mapping. */
static void shared_memory(void) {
    int fd = open("/dev/shm/desktop-test", O_RDWR | O_CREAT | O_EXCL, 0600);
    CHECK("/dev/shm open", fd >= 0);
    CHECK("/dev/shm truncate", ftruncate(fd, 4096) == 0);
    volatile int *p = mmap(NULL, 4096, PROT_READ | PROT_WRITE, MAP_SHARED, fd, 0);
    CHECK("/dev/shm mmap", p != MAP_FAILED);
    p[0] = 0;
    pid_t pid = fork();
    if (pid == 0) {
        p[0] = 1234;
        _exit(0);
    }
    int st;
    waitpid(pid, &st, 0);
    CHECK("shared across fork", p[0] == 1234);
    munmap((void *)p, 4096);
    close(fd);
    CHECK("/dev/shm unlink", unlink("/dev/shm/desktop-test") == 0);
}

struct ev { int mask; char name[64]; };

static int read_events(int ifd, struct ev *out, int max) {
    char buf[4096] __attribute__((aligned(8)));
    int n = 0;
    struct pollfd pfd = { ifd, POLLIN, 0 };
    while (n < max && poll(&pfd, 1, 200) == 1) {
        ssize_t len = read(ifd, buf, sizeof buf);
        if (len <= 0)
            break;
        for (char *p = buf; p < buf + len && n < max;) {
            struct inotify_event *e = (struct inotify_event *)p;
            out[n].mask = e->mask;
            snprintf(out[n].name, sizeof out[n].name, "%s", e->len ? e->name : "");
            n++;
            p += sizeof *e + e->len;
        }
    }
    return n;
}

static int has(struct ev *e, int n, int mask, const char *name) {
    for (int i = 0; i < n; i++)
        if ((e[i].mask & mask) == mask && !strcmp(e[i].name, name))
            return 1;
    return 0;
}

static void inotify(void) {
    mkdir("/tmp/watched", 0755);
    int ifd = inotify_init1(IN_NONBLOCK | IN_CLOEXEC);
    CHECK("inotify_init1", ifd >= 0);
    int wd = inotify_add_watch(ifd, "/tmp/watched", IN_ALL_EVENTS);
    CHECK("add_watch", wd > 0);
    char b[16];
    CHECK("empty", read(ifd, b, sizeof b) < 0 && errno == EAGAIN);
    int fd = open("/tmp/watched/a.txt", O_WRONLY | O_CREAT, 0644);
    write(fd, "hello", 5);
    close(fd);
    rename("/tmp/watched/a.txt", "/tmp/watched/b.txt");
    mkdir("/tmp/watched/sub", 0755);
    unlink("/tmp/watched/b.txt");
    struct ev e[32];
    int n = read_events(ifd, e, 32);
    CHECK("IN_CREATE", has(e, n, IN_CREATE, "a.txt"));
    CHECK("IN_MODIFY", has(e, n, IN_MODIFY, "a.txt"));
    CHECK("IN_CLOSE_WRITE", has(e, n, IN_CLOSE_WRITE, "a.txt"));
    CHECK("IN_MOVED_FROM", has(e, n, IN_MOVED_FROM, "a.txt"));
    CHECK("IN_MOVED_TO", has(e, n, IN_MOVED_TO, "b.txt"));
    CHECK("IN_CREATE|IN_ISDIR", has(e, n, IN_CREATE | IN_ISDIR, "sub"));
    CHECK("IN_DELETE", has(e, n, IN_DELETE, "b.txt"));
    printf("inotify: %d events\n", n);
    /* A watch on a file; deleting it ends the watch. */
    fd = open("/tmp/watched/c", O_WRONLY | O_CREAT, 0644);
    int wf = inotify_add_watch(ifd, "/tmp/watched/c", IN_MODIFY | IN_DELETE_SELF);
    read_events(ifd, e, 32);
    write(fd, "x", 1);
    close(fd);
    unlink("/tmp/watched/c");
    n = read_events(ifd, e, 32);
    int self_mod = 0, self_del = 0, ignored = 0;
    for (int i = 0; i < n; i++) {
        if (e[i].mask & IN_MODIFY && !e[i].name[0]) self_mod = 1;
        if (e[i].mask & IN_DELETE_SELF) self_del = 1;
        if (e[i].mask & IN_IGNORED) ignored = 1;
    }
    CHECK("file IN_MODIFY", self_mod);
    CHECK("IN_DELETE_SELF", self_del);
    CHECK("IN_IGNORED", ignored);
    CHECK("rm_watch", inotify_rm_watch(ifd, wd) == 0);
    CHECK("rm_watch twice", inotify_rm_watch(ifd, wf) < 0 && errno == EINVAL);
    rmdir("/tmp/watched/sub");
    rmdir("/tmp/watched");
    close(ifd);
}

static void pidfds(void) {
    pid_t pid = fork();
    if (pid == 0) {
        pause();
        _exit(0);
    }
    int pfd = syscall(SYS_pidfd_open, pid, 0);
    CHECK("pidfd_open", pfd >= 0);
    struct pollfd p = { pfd, POLLIN, 0 };
    CHECK("pidfd not ready", poll(&p, 1, 50) == 0);
    CHECK("pidfd_send_signal", syscall(SYS_pidfd_send_signal, pfd, SIGTERM, NULL, 0) == 0);
    CHECK("pidfd ready on exit", poll(&p, 1, 2000) == 1 && (p.revents & POLLIN));
    int st;
    CHECK("reaped", waitpid(pid, &st, 0) == pid && WIFSIGNALED(st) && WTERMSIG(st) == SIGTERM);
    CHECK("cloexec", fcntl(pfd, F_GETFD) == FD_CLOEXEC);
    close(pfd);
}

static void fd_ranges(void) {
    int a = open("/dev/null", O_RDONLY), b = dup(a), c = dup(a);
    CHECK("close_range cloexec", syscall(SYS_close_range, b, c, 4 /* CLOEXEC */) == 0 && fcntl(c, F_GETFD) == FD_CLOEXEC);
    CHECK("close_range", syscall(SYS_close_range, a, c, 0) == 0);
    CHECK("closed", fcntl(a, F_GETFD) < 0 && fcntl(c, F_GETFD) < 0);

    int in = open("/tmp/cfr-in", O_RDWR | O_CREAT | O_TRUNC, 0644);
    int out = open("/tmp/cfr-out", O_RDWR | O_CREAT | O_TRUNC, 0644);
    char data[100000];
    for (unsigned i = 0; i < sizeof data; i++)
        data[i] = i * 7;
    write(in, data, sizeof data);
    loff_t off = 10;
    ssize_t n = copy_file_range(in, &off, out, NULL, sizeof data - 10, 0);
    CHECK("copy_file_range", n == (ssize_t)sizeof data - 10 && off == (loff_t)sizeof data);
    char back[sizeof data];
    CHECK("copied data", pread(out, back, sizeof back, 0) == n && !memcmp(back, data + 10, n));
    CHECK("out offset", lseek(out, 0, SEEK_CUR) == n);
    close(in);
    close(out);
    unlink("/tmp/cfr-in");
    unlink("/tmp/cfr-out");
}

static void clocks(void) {
    struct timespec a, b, r;
    CHECK("BOOTTIME", clock_gettime(CLOCK_BOOTTIME, &a) == 0 && a.tv_sec >= 0);
    CHECK("MONOTONIC_RAW", clock_gettime(CLOCK_MONOTONIC_RAW, &b) == 0);
    clock_gettime(CLOCK_MONOTONIC, &a);
    r = a;
    r.tv_nsec += 50 * 1000000;
    if (r.tv_nsec >= 1000000000) { r.tv_sec++; r.tv_nsec -= 1000000000; }
    CHECK("clock_nanosleep abs", clock_nanosleep(CLOCK_MONOTONIC, TIMER_ABSTIME, &r, NULL) == 0);
    clock_gettime(CLOCK_MONOTONIC, &b);
    long ms = (b.tv_sec - a.tv_sec) * 1000 + (b.tv_nsec - a.tv_nsec) / 1000000;
    CHECK("slept until", ms >= 49 && ms < 1000);
    struct statx sx;
    CHECK("statx", statx(AT_FDCWD, "/bin", 0, STATX_BASIC_STATS, &sx) == 0 && S_ISDIR(sx.stx_mode));
}

/* Print the next kernel uevent (as udev would receive it). */
static int uevent(int secs) {
    int s = socket(AF_NETLINK, SOCK_DGRAM | SOCK_CLOEXEC, NETLINK_KOBJECT_UEVENT);
    if (s < 0) {
        perror("socket");
        return 1;
    }
    struct sockaddr_nl a = { .nl_family = AF_NETLINK, .nl_groups = 1 };
    if (bind(s, (struct sockaddr *)&a, sizeof a)) {
        perror("bind");
        return 1;
    }
    printf("uevent: listening\n");
    fflush(stdout);
    struct pollfd p = { s, POLLIN, 0 };
    int got = 0;
    while (poll(&p, 1, secs * 1000) == 1) {
        char buf[4096];
        ssize_t n = recv(s, buf, sizeof buf - 1, 0);
        if (n <= 0)
            break;
        buf[n] = 0;
        printf("uevent: %s\n", buf);
        for (char *q = buf + strlen(buf) + 1; q < buf + n; q += strlen(q) + 1)
            printf("  %s\n", q);
        fflush(stdout);
        got++;
        secs = 1;
    }
    printf("uevent: %d events\n", got);
    return got ? 0 : 1;
}

static volatile sig_atomic_t got_release, got_acquire;
static void on_release(int s) { (void)s; got_release = 1; }
static void on_acquire(int s) { (void)s; got_acquire = 1; }

static int active_vt(void) {
    struct vt_stat st;
    return ioctl(0, VT_GETSTATE, &st) ? -1 : st.v_active;
}

#define DRM_IOCTL_SET_MASTER _IO('d', 0x1e)
#define DRM_IOCTL_DROP_MASTER _IO('d', 0x1f)

static int vt(void) {
    signal(SIGUSR1, on_release);
    signal(SIGUSR2, on_acquire);
    int drm = open("/dev/dri/card0", O_RDWR | O_CLOEXEC);
    printf("vt: DRM %s\n", drm >= 0 ? "open" : "absent");
    int start = active_vt();
    CHECK("on vt 1", start == 1);
    struct vt_mode m = { .mode = VT_PROCESS, .relsig = SIGUSR1, .acqsig = SIGUSR2 };
    CHECK("VT_SETMODE", ioctl(0, VT_SETMODE, &m) == 0);
    CHECK("KDSKBMODE K_OFF", ioctl(0, KDSKBMODE, K_OFF) == 0);
    int kb = -1;
    CHECK("KDGKBMODE", ioctl(0, KDGKBMODE, &kb) == 0 && kb == K_OFF);
    CHECK("KD_GRAPHICS", ioctl(0, KDSETMODE, KD_GRAPHICS) == 0);
    /* Someone asks for vt 2: we get the release signal, and the switch
     * waits for us. */
    CHECK("VT_ACTIVATE 2", ioctl(0, VT_ACTIVATE, 2) == 0);
    for (int i = 0; i < 100 && !got_release; i++)
        usleep(10000);
    CHECK("release signal", got_release);
    CHECK("still on vt 1", active_vt() == 1);
    if (drm >= 0)
        CHECK("drop master", ioctl(drm, DRM_IOCTL_DROP_MASTER, 0) == 0);
    CHECK("VT_RELDISP allow", ioctl(0, VT_RELDISP, 1) == 0);
    CHECK("VT_WAITACTIVE 2", ioctl(0, VT_WAITACTIVE, 2) == 0 && active_vt() == 2);
    printf("vt: switched to %d\n", active_vt());
    /* Back to our console: the acquire signal. */
    CHECK("VT_ACTIVATE 1", ioctl(0, VT_ACTIVATE, 1) == 0);
    CHECK("VT_WAITACTIVE 1", ioctl(0, VT_WAITACTIVE, 1) == 0);
    for (int i = 0; i < 100 && !got_acquire; i++)
        usleep(10000);
    CHECK("acquire signal", got_acquire);
    CHECK("VT_RELDISP ack", ioctl(0, VT_RELDISP, VT_ACKACQ) == 0);
    if (drm >= 0)
        CHECK("set master", ioctl(drm, DRM_IOCTL_SET_MASTER, 0) == 0);
    /* A refused switch stays put. */
    got_release = 0;
    CHECK("VT_ACTIVATE 3", ioctl(0, VT_ACTIVATE, 3) == 0);
    for (int i = 0; i < 100 && !got_release; i++)
        usleep(10000);
    CHECK("VT_RELDISP refuse", got_release && ioctl(0, VT_RELDISP, 0) == 0);
    usleep(50000);
    CHECK("refused", active_vt() == 1);
    /* Back to how a shell expects the console. */
    m.mode = VT_AUTO;
    CHECK("VT_AUTO", ioctl(0, VT_SETMODE, &m) == 0);
    CHECK("KD_TEXT", ioctl(0, KDSETMODE, KD_TEXT) == 0);
    CHECK("K_UNICODE", ioctl(0, KDSKBMODE, K_UNICODE) == 0);
    printf("musl-desktop vt: %d passed, %d failed\n", passed, failed);
    return failed != 0;
}

int main(int argc, char **argv) {
    setvbuf(stdout, NULL, _IONBF, 0);
    if (argc == 3 && !strcmp(argv[1], "uevent"))
        return uevent(atoi(argv[2]));
    if (argc == 2 && !strcmp(argv[1], "vt"))
        return vt();
    memfd_passing();
    shared_memory();
    inotify();
    pidfds();
    fd_ranges();
    clocks();
    printf("musl-desktop: %d passed, %d failed\n", passed, failed);
    return failed != 0;
}
