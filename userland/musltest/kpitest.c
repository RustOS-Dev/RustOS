/* musl-kpitest: drive /dev/kpi-test (src/linuxkpi/c/testdev.c, kernels
 * built with --features linux-test) to check LinuxKPI's character-device
 * bridge: read/write, ioctl through user pointers, poll wake-ups and both
 * kinds of mmap. */
#include <errno.h>
#include <fcntl.h>
#include <poll.h>
#include <stdio.h>
#include <string.h>
#include <sys/ioctl.h>
#include <sys/mman.h>
#include <sys/stat.h>
#include <sys/sysmacros.h>
#include <unistd.h>

#define KPI_TEST_IOC_GET	_IOWR('k', 1, int)
#define KPI_TEST_IOC_ARM	_IO('k', 2)

static int passed, failed;
#define CHECK(name, cond) do { if (cond) passed++; else { failed++; printf("FAIL %s (line %d, errno %d)\n", name, __LINE__, errno); } } while (0)

int main(void)
{
	char buf[64];
	struct stat st;
	int fd = open("/dev/kpi-test", O_RDWR), v = 1;

	if (fd < 0) {
		printf("kpitest: no /dev/kpi-test (kernel without linux-test)\n");
		return 2;
	}
	CHECK("stat", fstat(fd, &st) == 0 && S_ISCHR(st.st_mode) && major(st.st_rdev) == 10);
	ssize_t n = read(fd, buf, sizeof(buf));
	CHECK("read", n == 9 && !memcmp(buf, "linuxkpi\n", 9));
	CHECK("write", write(fd, "hello", 5) == 5);
	CHECK("lseek", lseek(fd, 0, SEEK_SET) == 0 || errno == ESPIPE);
	int fd2 = open("/dev/kpi-test", O_RDONLY);
	n = read(fd2, buf, sizeof(buf));
	CHECK("read back", n == 5 && !memcmp(buf, "hello", 5));
	close(fd2);
	CHECK("ioctl", ioctl(fd, KPI_TEST_IOC_GET, &v) == 0 && v == 43);
	CHECK("ioctl bad pointer", ioctl(fd, KPI_TEST_IOC_GET, (int *)0x10) < 0 && errno == EFAULT);
	CHECK("ioctl unknown", ioctl(fd, _IO('k', 9)) < 0 && errno == ENOTTY);

	struct pollfd p = { .fd = fd, .events = POLLIN };
	CHECK("arm", ioctl(fd, KPI_TEST_IOC_ARM) == 0);
	CHECK("poll not ready", poll(&p, 1, 0) == 0);
	CHECK("poll wakes", poll(&p, 1, 2000) == 1 && (p.revents & POLLIN));

	char *m0 = mmap(NULL, 4096, PROT_READ | PROT_WRITE, MAP_SHARED, fd, 0);
	char *m1 = mmap(NULL, 4096, PROT_READ, MAP_SHARED, fd, 4096);
	CHECK("mmap remap_pfn_range", m0 != MAP_FAILED && !strcmp(m0, "kpi page 0"));
	CHECK("mmap fault", m1 != MAP_FAILED && !strcmp(m1, "kpi page 1"));
	if (m0 != MAP_FAILED) {
		strcpy(m0, "written");
		munmap(m0, 4096);
		m0 = mmap(NULL, 4096, PROT_READ, MAP_SHARED, fd, 0);
		CHECK("mmap shared write", m0 != MAP_FAILED && !strcmp(m0, "written"));
	}
	close(fd);
	printf("kpitest: %d passed, %d failed\n", passed, failed);
	return failed != 0;
}
