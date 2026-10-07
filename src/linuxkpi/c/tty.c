// SPDX-License-Identifier: GPL-2.0-or-later
/*
 * LinuxKPI tty drivers: the parts of drivers/tty/tty_io.c that serial
 * drivers (usb-serial, cdc-acm) rely on, on top of RustOS terminals.
 *
 * Every registered tty device (/dev/ttyUSB0, ...) is a RustOS terminal
 * (src/tty.rs) whose line discipline is RustOS's: opening it opens the
 * Linux tty (install + ops->open), writes go to ops->write, termios
 * changes to ops->set_termios. Received characters go through Linux's
 * flip buffers (tty_buffer.c, imported) to the port's client operations
 * here, which hand them to the RustOS terminal. tty_port.c (imported)
 * keeps Linux's open/close/hangup rules.
 */
#include <linux/device.h>
#include <linux/module.h>
#include <linux/slab.h>
#include <linux/tty.h>
#include <linux/tty_driver.h>
#include <linux/tty_flip.h>
#include <linux/tty_ldisc.h>
#include <linux/termios_internal.h>
#include "tty.h"	/* drivers/tty/tty.h */
#include "kpi.h"

#define KPI_MAX_TTYS 64

/* One registered tty device. */
struct kpi_tty {
	struct tty_driver *driver;
	int index;
	u64 handle;		/* RustOS terminal */
	struct tty_struct *tty;	/* while open */
	struct device *dev;
	struct mutex lock;
};

static struct kpi_tty *kpi_ttys[KPI_MAX_TTYS];
static DEFINE_MUTEX(kpi_tty_lock);

struct ktermios tty_std_termios = {
	.c_iflag = ICRNL | IXON,
	.c_oflag = OPOST | ONLCR,
	.c_cflag = B38400 | CS8 | CREAD | HUPCL,
	.c_lflag = ISIG | ICANON | ECHO | ECHOE | ECHOK | ECHOCTL | ECHOKE | IEXTEN,
	.c_cc = INIT_C_CC,
	.c_ispeed = 38400,
	.c_ospeed = 38400,
};
EXPORT_SYMBOL(tty_std_termios);

static struct kpi_tty *kpi_tty_find(struct tty_driver *driver, int index)
{
	for (int i = 0; i < KPI_MAX_TTYS; i++)
		if (kpi_ttys[i] && kpi_ttys[i]->driver == driver && kpi_ttys[i]->index == index)
			return kpi_ttys[i];
	return NULL;
}

/* ---------------------------------------------------------------- drivers */

struct tty_driver *__tty_alloc_driver(unsigned int lines, struct module *owner,
				      unsigned long flags)
{
	struct tty_driver *driver;

	if (!lines || (flags & TTY_DRIVER_UNNUMBERED_NODE && lines > 1))
		return ERR_PTR(-EINVAL);
	driver = kzalloc(sizeof(*driver), GFP_KERNEL);
	if (!driver)
		return ERR_PTR(-ENOMEM);
	kref_init(&driver->kref);
	driver->num = lines;
	driver->owner = owner;
	driver->flags = flags;
	driver->ttys = kcalloc(lines, sizeof(*driver->ttys), GFP_KERNEL);
	driver->termios = kcalloc(lines, sizeof(*driver->termios), GFP_KERNEL);
	if (!(flags & TTY_DRIVER_DEVPTS_MEM))
		driver->ports = kcalloc(lines, sizeof(*driver->ports), GFP_KERNEL);
	if (!driver->ttys || !driver->termios || (!(flags & TTY_DRIVER_DEVPTS_MEM) && !driver->ports)) {
		kfree(driver->ttys);
		kfree(driver->termios);
		kfree(driver->ports);
		kfree(driver);
		return ERR_PTR(-ENOMEM);
	}
	return driver;
}
EXPORT_SYMBOL(__tty_alloc_driver);

static void kpi_destruct_driver(struct kref *kref)
{
	struct tty_driver *driver = container_of(kref, struct tty_driver, kref);

	for (unsigned int i = 0; i < driver->num; i++)
		kfree(driver->termios[i]);
	kfree(driver->ports);
	kfree(driver->termios);
	kfree(driver->ttys);
	kfree(driver);
}

void tty_driver_kref_put(struct tty_driver *driver)
{
	kref_put(&driver->kref, kpi_destruct_driver);
}
EXPORT_SYMBOL(tty_driver_kref_put);

int tty_register_driver(struct tty_driver *driver)
{
	if (!(driver->flags & TTY_DRIVER_DYNAMIC_DEV))
		for (unsigned int i = 0; i < driver->num; i++) {
			struct device *d = tty_register_device(driver, i, NULL);

			if (IS_ERR(d))
				return PTR_ERR(d);
		}
	driver->flags |= TTY_DRIVER_INSTALLED;
	return 0;
}
EXPORT_SYMBOL(tty_register_driver);

void tty_unregister_driver(struct tty_driver *driver)
{
	if (!(driver->flags & TTY_DRIVER_DYNAMIC_DEV))
		for (unsigned int i = 0; i < driver->num; i++)
			tty_unregister_device(driver, i);
	driver->flags &= ~TTY_DRIVER_INSTALLED;
}
EXPORT_SYMBOL(tty_unregister_driver);

static void kpi_tty_dev_release(struct device *dev)
{
	kfree(dev);
}

struct device *tty_register_device_attr(struct tty_driver *driver, unsigned index,
					struct device *device, void *drvdata,
					const struct attribute_group **attr_grp)
{
	struct kpi_tty *kt;
	struct device *dev;
	char name[64];
	int slot = -1;

	if (index >= driver->num)
		return ERR_PTR(-EINVAL);
	if (driver->flags & TTY_DRIVER_UNNUMBERED_NODE)
		strscpy(name, driver->name, sizeof(name));
	else
		snprintf(name, sizeof(name), "%s%d", driver->name, index + driver->name_base);
	kt = kzalloc(sizeof(*kt), GFP_KERNEL);
	dev = kzalloc(sizeof(*dev), GFP_KERNEL);
	if (!kt || !dev) {
		kfree(kt);
		kfree(dev);
		return ERR_PTR(-ENOMEM);
	}
	kt->driver = driver;
	kt->index = index;
	mutex_init(&kt->lock);
	device_initialize(dev);
	dev->parent = device;
	dev->devt = MKDEV(driver->major, driver->minor_start + index);
	dev->release = kpi_tty_dev_release;
	dev_set_drvdata(dev, drvdata);
	dev_set_name(dev, "%s", name);
	kt->dev = dev;
	mutex_lock(&kpi_tty_lock);
	for (int i = 0; i < KPI_MAX_TTYS; i++)
		if (!kpi_ttys[i]) {
			slot = i;
			kpi_ttys[i] = kt;
			break;
		}
	mutex_unlock(&kpi_tty_lock);
	if (slot < 0) {
		put_device(dev);
		kfree(kt);
		return ERR_PTR(-ENFILE);
	}
	kt->handle = rustos_kpi_tty_register(name, driver->major, driver->minor_start + index,
					     kt, driver->init_termios.c_cflag);
	pr_info("%s: %s\n", driver->driver_name ? driver->driver_name : driver->name, name);
	return dev;
}
EXPORT_SYMBOL_GPL(tty_register_device_attr);

struct device *tty_register_device(struct tty_driver *driver, unsigned index,
				   struct device *device)
{
	return tty_register_device_attr(driver, index, device, NULL, NULL);
}
EXPORT_SYMBOL(tty_register_device);

void tty_unregister_device(struct tty_driver *driver, unsigned index)
{
	struct kpi_tty *kt;

	mutex_lock(&kpi_tty_lock);
	kt = kpi_tty_find(driver, index);
	for (int i = 0; kt && i < KPI_MAX_TTYS; i++)
		if (kpi_ttys[i] == kt)
			kpi_ttys[i] = NULL;
	mutex_unlock(&kpi_tty_lock);
	if (!kt)
		return;
	/* The terminal hangs up; an open file keeps the RustOS side, whose
	 * close then finds no Linux tty (handle 0 below). */
	rustos_kpi_tty_unregister(kt->handle);
	put_device(kt->dev);
	mutex_lock(&kt->lock);
	if (!kt->tty) {
		mutex_unlock(&kt->lock);
		kfree(kt);
		return;
	}
	kt->handle = 0;	/* freed by the last close */
	mutex_unlock(&kt->lock);
}
EXPORT_SYMBOL(tty_unregister_device);

/* ---------------------------------------------------------- tty structs */

static void kpi_release_tty(struct kref *kref)
{
	struct tty_struct *tty = container_of(kref, struct tty_struct, kref);

	if (tty->ops->shutdown)
		tty->ops->shutdown(tty);
	if (tty->ops->cleanup)
		tty->ops->cleanup(tty);
	if (tty->port)
		tty_port_put(tty->port);
	tty_driver_kref_put(tty->driver);
	kfree(tty);
}

void tty_kref_put(struct tty_struct *tty)
{
	if (tty)
		kref_put(&tty->kref, kpi_release_tty);
}
EXPORT_SYMBOL(tty_kref_put);

void tty_init_termios(struct tty_struct *tty)
{
	struct ktermios *tp = tty->driver->termios[tty->index];

	tty->termios = tp ? *tp : tty->driver->init_termios;
	if (!tp) {
		tty->termios.c_ispeed = tty_termios_input_baud_rate(&tty->termios);
		tty->termios.c_ospeed = tty_termios_baud_rate(&tty->termios);
	}
}
EXPORT_SYMBOL_GPL(tty_init_termios);

int tty_standard_install(struct tty_driver *driver, struct tty_struct *tty)
{
	tty_init_termios(tty);
	tty_driver_kref_get(driver);
	tty->count++;
	driver->ttys[tty->index] = tty;
	return 0;
}
EXPORT_SYMBOL_GPL(tty_standard_install);

const char *tty_name(const struct tty_struct *tty)
{
	return tty ? tty->name : "NULL tty";
}
EXPORT_SYMBOL(tty_name);

const char *tty_driver_name(const struct tty_struct *tty)
{
	return tty && tty->driver ? tty->driver->name : "";
}

dev_t tty_devnum(struct tty_struct *tty)
{
	return MKDEV(tty->driver->major, tty->driver->minor_start) + tty->index;
}
EXPORT_SYMBOL(tty_devnum);

int tty_hung_up_p(struct file *filp)
{
	return 0;
}
EXPORT_SYMBOL(tty_hung_up_p);

static struct kpi_tty *kpi_of(struct tty_struct *tty)
{
	return tty ? tty->disc_data : NULL;
}

void tty_hangup(struct tty_struct *tty)
{
	struct kpi_tty *kt = kpi_of(tty);

	if (kt && kt->handle)
		rustos_kpi_tty_hangup(kt->handle);
}
EXPORT_SYMBOL(tty_hangup);

void tty_vhangup(struct tty_struct *tty)
{
	tty_hangup(tty);
}
EXPORT_SYMBOL(tty_vhangup);

void tty_lock(struct tty_struct *tty)
{
	tty_kref_get(tty);
	mutex_lock(&tty->legacy_mutex);
}

void tty_unlock(struct tty_struct *tty)
{
	mutex_unlock(&tty->legacy_mutex);
	tty_kref_put(tty);
}

void tty_wakeup(struct tty_struct *tty)
{
	wake_up_all(&tty->write_wait);
}
EXPORT_SYMBOL_GPL(tty_wakeup);

void tty_driver_flush_buffer(struct tty_struct *tty)
{
	if (tty->ops->flush_buffer)
		tty->ops->flush_buffer(tty);
}
EXPORT_SYMBOL(tty_driver_flush_buffer);

void tty_wait_until_sent(struct tty_struct *tty, long timeout)
{
	if (!timeout)
		timeout = MAX_SCHEDULE_TIMEOUT;
	wait_event_interruptible_timeout(tty->write_wait,
					 !tty->ops->chars_in_buffer ||
					 !tty->ops->chars_in_buffer(tty), timeout);
	if (tty->ops->wait_until_sent)
		tty->ops->wait_until_sent(tty, timeout);
}
EXPORT_SYMBOL(tty_wait_until_sent);

/* Line disciplines are RustOS's: Linux code sees none. */
struct tty_ldisc *tty_ldisc_ref(struct tty_struct *tty)
{
	return NULL;
}
EXPORT_SYMBOL_GPL(tty_ldisc_ref);

struct tty_ldisc *tty_ldisc_ref_wait(struct tty_struct *tty)
{
	return NULL;
}
EXPORT_SYMBOL_GPL(tty_ldisc_ref_wait);

void tty_ldisc_deref(struct tty_ldisc *ld)
{
}
EXPORT_SYMBOL_GPL(tty_ldisc_deref);

void tty_ldisc_flush(struct tty_struct *tty)
{
	tty_buffer_flush(tty, NULL);
}
EXPORT_SYMBOL_GPL(tty_ldisc_flush);

/* drivers/tty/tty_ioctl.c */
void tty_termios_copy_hw(struct ktermios *new, const struct ktermios *old)
{
	new->c_cflag &= HUPCL | CREAD | CLOCAL;
	new->c_cflag |= old->c_cflag & ~(HUPCL | CREAD | CLOCAL);
	new->c_ispeed = old->c_ispeed;
	new->c_ospeed = old->c_ospeed;
}
EXPORT_SYMBOL(tty_termios_copy_hw);

bool tty_termios_hw_change(const struct ktermios *a, const struct ktermios *b)
{
	if (a->c_ispeed != b->c_ispeed || a->c_ospeed != b->c_ospeed)
		return true;
	if ((a->c_cflag ^ b->c_cflag) & ~(HUPCL | CREAD | CLOCAL))
		return true;
	return false;
}
EXPORT_SYMBOL(tty_termios_hw_change);

unsigned char tty_get_char_size(unsigned int cflag)
{
	switch (cflag & CSIZE) {
	case CS5:
		return 5;
	case CS6:
		return 6;
	case CS7:
		return 7;
	case CS8:
	default:
		return 8;
	}
}
EXPORT_SYMBOL_GPL(tty_get_char_size);

int tty_put_char(struct tty_struct *tty, u8 ch)
{
	if (tty->ops->put_char)
		return tty->ops->put_char(tty, ch);
	return tty->ops->write(tty, &ch, 1);
}
EXPORT_SYMBOL_GPL(tty_put_char);

/* ------------------------------------------------ receive path (flip) */

static size_t kpi_port_receive_buf(struct tty_port *port, const u8 *p, const u8 *f,
				   size_t count)
{
	struct kpi_tty *kt = port->client_data;

	if (kt && kt->handle)
		rustos_kpi_tty_receive(kt->handle, p, count);
	return count;
}

static void kpi_port_write_wakeup(struct tty_port *port)
{
	struct tty_struct *tty = tty_port_tty_get(port);

	if (tty) {
		tty_wakeup(tty);
		tty_kref_put(tty);
	}
}

static const struct tty_port_client_operations kpi_port_client_ops = {
	.receive_buf = kpi_port_receive_buf,
	.write_wakeup = kpi_port_write_wakeup,
};

/* -------------------------------------------------- calls from RustOS */

/* First open of the terminal: install and open the Linux tty. */
int kpi_tty_open(struct kpi_tty *kt)
{
	struct tty_driver *driver = kt->driver;
	struct tty_struct *tty;
	int err;

	mutex_lock(&kt->lock);
	if (kt->tty) {
		mutex_unlock(&kt->lock);
		return 0;
	}
	tty = kzalloc(sizeof(*tty), GFP_KERNEL);
	if (!tty) {
		mutex_unlock(&kt->lock);
		return -ENOMEM;
	}
	kref_init(&tty->kref);
	tty->index = kt->index;
	tty->driver = driver;
	tty->ops = driver->ops;
	tty->dev = kt->dev;
	tty->disc_data = kt;
	mutex_init(&tty->atomic_write_lock);
	mutex_init(&tty->legacy_mutex);
	mutex_init(&tty->throttle_mutex);
	init_rwsem(&tty->termios_rwsem);
	mutex_init(&tty->winsize_mutex);
	spin_lock_init(&tty->flow.lock);
	spin_lock_init(&tty->ctrl.lock);
	spin_lock_init(&tty->files_lock);
	init_waitqueue_head(&tty->write_wait);
	init_waitqueue_head(&tty->read_wait);
	INIT_LIST_HEAD(&tty->tty_files);
	tty->receive_room = 4096;	/* n_tty's buffer size */
	snprintf(tty->name, sizeof(tty->name), "%s%d", driver->name,
		 kt->index + driver->name_base);
	if (driver->ports && driver->ports[kt->index])
		tty->port = driver->ports[kt->index];
	err = driver->ops->install ? driver->ops->install(driver, tty)
				   : tty_standard_install(driver, tty);
	if (err) {
		kfree(tty);
		mutex_unlock(&kt->lock);
		return err;
	}
	if (!tty->port && driver->ports)
		tty->port = driver->ports[kt->index];
	if (tty->port) {
		tty_port_get(tty->port);
		tty->port->client_ops = &kpi_port_client_ops;
		tty->port->client_data = kt;
		tty->port->itty = tty;
	}
	err = tty->ops->open ? tty->ops->open(tty, NULL) : -ENODEV;
	if (err) {
		if (tty->ops->close)
			tty->ops->close(tty, NULL);
		driver->ttys[tty->index] = NULL;
		tty_kref_put(tty);
		mutex_unlock(&kt->lock);
		return err;
	}
	kt->tty = tty;
	mutex_unlock(&kt->lock);
	return 0;
}

/* Last close. */
void kpi_tty_close(struct kpi_tty *kt)
{
	struct tty_struct *tty;
	bool gone;

	mutex_lock(&kt->lock);
	tty = kt->tty;
	kt->tty = NULL;
	gone = !kt->handle;
	mutex_unlock(&kt->lock);
	if (tty) {
		if (tty->ops->close)
			tty->ops->close(tty, NULL);
		tty->count = 0;
		tty_buffer_cancel_work(tty->port);
		if (tty->port) {
			tty->port->itty = NULL;
			tty->port->client_data = NULL;
		}
		tty->driver->ttys[tty->index] = NULL;
		/* Keep the settings for the next open, as Linux does. */
		if (!tty->driver->termios[tty->index])
			tty->driver->termios[tty->index] = kmalloc(sizeof(struct ktermios), GFP_KERNEL);
		if (tty->driver->termios[tty->index])
			*tty->driver->termios[tty->index] = tty->termios;
		tty_kref_put(tty);
	}
	if (gone)
		kfree(kt);
}

/* Write some of `buf`, waiting (up to 10 s) for room. */
int kpi_tty_write(struct kpi_tty *kt, const u8 *buf, u32 len)
{
	struct tty_struct *tty = kt->tty;
	unsigned int room;
	long left;

	if (!tty || !tty->ops->write)
		return -EIO;
	left = wait_event_interruptible_timeout(tty->write_wait,
		(room = tty->ops->write_room ? tty->ops->write_room(tty) : len) > 0 ||
		test_bit(TTY_IO_ERROR, &tty->flags) || !kt->handle,
		10 * HZ);
	if (left < 0)
		return left;
	if (!left || !room || !kt->handle || test_bit(TTY_IO_ERROR, &tty->flags))
		return -EIO;
	return tty->ops->write(tty, buf, min_t(u32, len, room));
}

/* New line settings: the c_iflag..c_cc part of struct termios. */
void kpi_tty_set_termios(struct kpi_tty *kt, const u32 *flags, const u8 *cc)
{
	struct tty_struct *tty = kt->tty;
	struct ktermios old;

	if (!tty)
		return;
	down_write(&tty->termios_rwsem);
	old = tty->termios;
	tty->termios.c_iflag = flags[0];
	tty->termios.c_oflag = flags[1];
	tty->termios.c_cflag = flags[2];
	tty->termios.c_lflag = flags[3];
	memcpy(tty->termios.c_cc, cc, min_t(size_t, NCCS, 19));
	tty->termios.c_ispeed = tty_termios_input_baud_rate(&tty->termios);
	tty->termios.c_ospeed = tty_termios_baud_rate(&tty->termios);
	if (tty->ops->set_termios && tty_termios_hw_change(&tty->termios, &old))
		tty->ops->set_termios(tty, &old);
	up_write(&tty->termios_rwsem);
}
