// SPDX-License-Identifier: GPL-2.0-or-later
/*
 * I/O vector iterators (lib/iov_iter.c), for the iterator kinds RustOS's
 * Linux code meets: a user buffer (ITER_UBUF), user iovecs (ITER_IOVEC)
 * and kernel kvecs (ITER_KVEC).
 */
#include <linux/fs.h>
#include <linux/kernel.h>
#include <linux/uaccess.h>
#include <linux/uio.h>
#include "kpi.h"

void iov_iter_kvec(struct iov_iter *i, unsigned int direction, const struct kvec *kvec,
		   unsigned long nr_segs, size_t count)
{
	WARN_ON(direction & ~(READ | WRITE));
	*i = (struct iov_iter){
		.iter_type = ITER_KVEC,
		.data_source = direction,
		.kvec = kvec,
		.nr_segs = nr_segs,
		.iov_offset = 0,
		.count = count,
	};
}

int import_ubuf(int rw, void __user *buf, size_t len, struct iov_iter *i)
{
	if (len > MAX_RW_COUNT)
		len = MAX_RW_COUNT;
	/* No access_ok(): RustOS passes drivers kernel bounce buffers as user
	 * buffers; copy_to_user()/copy_from_user() check the addresses. */
	iov_iter_ubuf(i, rw, buf, len);
	return 0;
}

/*
 * Copy up to @bytes between @buf and the iterator's current position
 * (to it if @to_iter), advancing it. Returns the bytes copied. A NULL
 * @buf only advances.
 */
static size_t kpi_iter_copy(struct iov_iter *i, void *buf, size_t bytes, bool to_iter)
{
	size_t done = 0;

	bytes = min(bytes, i->count);
	while (done < bytes) {
		size_t seg_len, n, left;
		void *base;
		bool user;

		switch (i->iter_type) {
		case ITER_UBUF:
			base = i->ubuf;
			seg_len = i->count + i->iov_offset;
			user = true;
			break;
		case ITER_IOVEC:
			if (!i->nr_segs)
				return done;
			base = i->__iov->iov_base;
			seg_len = i->__iov->iov_len;
			user = true;
			break;
		case ITER_KVEC:
			if (!i->nr_segs)
				return done;
			base = i->kvec->iov_base;
			seg_len = i->kvec->iov_len;
			user = false;
			break;
		default:
			WARN_ONCE(1, "iov_iter type %d not supported\n", i->iter_type);
			return done;
		}
		n = min(bytes - done, seg_len - i->iov_offset);
		if (!buf)
			left = 0;
		else if (user)
			left = to_iter ? copy_to_user(base + i->iov_offset, buf + done, n)
				       : copy_from_user(buf + done, base + i->iov_offset, n);
		else
			left = (to_iter ? memcpy(base + i->iov_offset, buf + done, n)
					: memcpy(buf + done, base + i->iov_offset, n), 0);
		n -= left;
		done += n;
		i->count -= n;
		i->iov_offset += n;
		if (i->iter_type != ITER_UBUF && i->iov_offset == seg_len) {
			i->iov_offset = 0;
			i->nr_segs--;
			if (i->iter_type == ITER_IOVEC)
				i->__iov++;
			else
				i->kvec++;
		}
		if (left)
			break;
	}
	return done;
}

size_t _copy_to_iter(const void *addr, size_t bytes, struct iov_iter *i)
{
	return kpi_iter_copy(i, (void *)addr, bytes, true);
}

size_t _copy_from_iter(void *addr, size_t bytes, struct iov_iter *i)
{
	return kpi_iter_copy(i, addr, bytes, false);
}

void iov_iter_advance(struct iov_iter *i, size_t bytes)
{
	kpi_iter_copy(i, NULL, bytes, false);
}

/* Pinning the pages behind an iterator (for zero-copy I/O) is not
 * supported; callers fall back to copying or fail the request. */
ssize_t iov_iter_extract_pages(struct iov_iter *i, struct page ***pages, size_t maxsize,
			       unsigned int maxpages, iov_iter_extraction_t extraction_flags,
			       size_t *offset0)
{
	return -EFAULT;
}
