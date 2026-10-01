// SPDX-License-Identifier: GPL-2.0-or-later
/*
 * sysfs for LinuxKPI: the fs/sysfs API the device core and drivers call,
 * publishing into RustOS's /sys registry (src/linuxkpi/sysfs.rs,
 * src/vfs/sysfs.rs).
 *
 * Each kobject directory gets a real struct kernfs_node (kobject.c and
 * core.c use its refcount and dir.subdirs), embedded in struct kpi_kn,
 * which also lists the attribute files created in it. A file is a
 * struct kpi_sysfs_file, handed to RustOS as the cookie that comes back
 * in kpi_sysfs_show()/kpi_sysfs_store(). RustOS drains running calls
 * before a removal returns, so a cookie can be freed right after.
 */
#include <linux/device.h>
#include <linux/kernfs.h>
#include <linux/kobject.h>
#include <linux/mm.h>
#include <linux/mutex.h>
#include <linux/slab.h>
#include <linux/string.h>
#include <linux/sysfs.h>
#include <linux/workqueue.h>
#include "kpi.h"

struct kpi_kn {
	struct kernfs_node kn;
	struct list_head files;
};

struct kpi_sysfs_file {
	struct list_head list;
	struct kobject *kobj;
	const struct attribute *attr;
	const struct bin_attribute *battr;
	const char *name;
};

static DEFINE_MUTEX(kpi_sysfs_lock);
static struct kpi_kn kpi_sysfs_root = {
	.kn.name = "",
	.kn.flags = KERNFS_DIR,
	.kn.count = ATOMIC_INIT(1),
	.files = LIST_HEAD_INIT(kpi_sysfs_root.files),
};

static struct kpi_kn *to_kpi_kn(struct kernfs_node *kn)
{
	return container_of(kn, struct kpi_kn, kn);
}

static struct kernfs_node *kpi_kn_parent(struct kernfs_node *kn)
{
	return rcu_dereference_raw(kn->__parent);
}

static const char *kpi_kn_name(struct kernfs_node *kn)
{
	return rcu_dereference_raw(kn->name);
}

/* "devices/platform[/child]" (relative to /sys); kmalloc'd. */
static char *kpi_kn_path(struct kernfs_node *kn, const char *child)
{
	char *buf = kmalloc(PATH_MAX, GFP_KERNEL), *p;
	size_t len;

	if (!buf)
		return NULL;
	p = buf + PATH_MAX - 1;
	*p = '\0';
	if (child) {
		len = strlen(child);
		if (len + 1 > (size_t)(p - buf))
			goto toolong;
		p -= len;
		memcpy(p, child, len);
	}
	for (; kn && kpi_kn_parent(kn); kn = kpi_kn_parent(kn)) {
		const char *name = kpi_kn_name(kn);

		len = strlen(name);
		if (*p) {
			if (p == buf)
				goto toolong;
			*--p = '/';
		}
		if (len > (size_t)(p - buf))
			goto toolong;
		p -= len;
		memcpy(p, name, len);
	}
	memmove(buf, p, strlen(p) + 1);
	return buf;
toolong:
	kfree(buf);
	return NULL;
}

static struct kernfs_node *kpi_kn_new(struct kernfs_node *parent, const char *name, void *priv)
{
	struct kpi_kn *k = kzalloc(sizeof(*k), GFP_KERNEL);

	if (!k)
		return NULL;
	k->kn.name = kstrdup(name, GFP_KERNEL);
	if (!k->kn.name) {
		kfree(k);
		return NULL;
	}
	atomic_set(&k->kn.count, 1);
	k->kn.flags = KERNFS_DIR;
	k->kn.mode = S_IFDIR | 0755;
	k->kn.priv = priv;
	INIT_LIST_HEAD(&k->files);
	kernfs_get(parent);
	rcu_assign_pointer(k->kn.__parent, parent);
	return &k->kn;
}

void kernfs_get(struct kernfs_node *kn)
{
	if (kn)
		atomic_inc(&kn->count);
}

void kernfs_put(struct kernfs_node *kn)
{
	while (kn && kn != &kpi_sysfs_root.kn && atomic_dec_and_test(&kn->count)) {
		struct kernfs_node *parent = kpi_kn_parent(kn);

		WARN_ON(!list_empty(&to_kpi_kn(kn)->files));
		kfree(kpi_kn_name(kn));
		kfree(to_kpi_kn(kn));
		kn = parent;
	}
}

static struct kernfs_node *kpi_kobj_dir(struct kobject *kobj)
{
	return kobj && kobj->sd ? kobj->sd : &kpi_sysfs_root.kn;
}

/* ------------------------------------------------------- show / store */

ssize_t kpi_sysfs_show(void *cookie, char *out)
{
	struct kpi_sysfs_file *f = cookie;
	char *page = (char *)get_zeroed_page(GFP_KERNEL);
	ssize_t r = -EIO;

	if (!page)
		return -ENOMEM;
	if (f->battr) {
		if (f->battr->read)
			r = f->battr->read(NULL, f->kobj, f->battr, page, 0, PAGE_SIZE);
	} else if (f->kobj->ktype && f->kobj->ktype->sysfs_ops &&
		   f->kobj->ktype->sysfs_ops->show) {
		r = f->kobj->ktype->sysfs_ops->show(f->kobj, (struct attribute *)f->attr, page);
	}
	if (r > 0)
		memcpy(out, page, min_t(size_t, r, PAGE_SIZE));
	free_page((unsigned long)page);
	return r;
}

ssize_t kpi_sysfs_store(void *cookie, const char *buf, size_t len)
{
	struct kpi_sysfs_file *f = cookie;

	if (f->battr)
		return f->battr->write ? f->battr->write(NULL, f->kobj, f->battr, (char *)buf, 0,
							 len) : -EIO;
	if (f->kobj->ktype && f->kobj->ktype->sysfs_ops && f->kobj->ktype->sysfs_ops->store)
		return f->kobj->ktype->sysfs_ops->store(f->kobj, (struct attribute *)f->attr, buf,
							 len);
	return -EIO;
}

int sysfs_emit(char *buf, const char *fmt, ...)
{
	va_list args;
	int len;

	if (WARN(!buf || offset_in_page(buf), "invalid sysfs_emit: buf:%p\n", buf))
		return 0;
	va_start(args, fmt);
	len = vscnprintf(buf, PAGE_SIZE, fmt, args);
	va_end(args);
	return len;
}

int sysfs_emit_at(char *buf, int at, const char *fmt, ...)
{
	va_list args;
	int len;

	if (WARN(!buf || offset_in_page(buf) || at < 0 || at >= PAGE_SIZE,
		 "invalid sysfs_emit_at: buf:%p at:%d\n", buf, at))
		return 0;
	va_start(args, fmt);
	len = vscnprintf(buf + at, PAGE_SIZE - at, fmt, args);
	va_end(args);
	return len;
}

/* ---------------------------------------------------------------- files */

static int kpi_add_file(struct kernfs_node *dir, struct kobject *kobj,
			const struct attribute *attr, const struct bin_attribute *battr,
			umode_t mode)
{
	struct kpi_sysfs_file *f;
	char *path;
	int err;

	f = kzalloc(sizeof(*f), GFP_KERNEL);
	if (!f)
		return -ENOMEM;
	f->kobj = kobj;
	f->attr = attr;
	f->battr = battr;
	f->name = attr->name;
	mutex_lock(&kpi_sysfs_lock);
	path = kpi_kn_path(dir, attr->name);
	err = path ? rustos_kpi_sysfs_add_file(path, mode & 0777, f) : -ENOMEM;
	if (!err)
		list_add_tail(&f->list, &to_kpi_kn(dir)->files);
	mutex_unlock(&kpi_sysfs_lock);
	if (err == -EEXIST)
		pr_warn("sysfs: cannot create duplicate filename '/sys/%s'\n", path);
	kfree(path);
	if (err)
		kfree(f);
	return err;
}

static void kpi_remove_file(struct kernfs_node *dir, const char *name)
{
	struct kpi_sysfs_file *f, *found = NULL;
	char *path;

	mutex_lock(&kpi_sysfs_lock);
	list_for_each_entry(f, &to_kpi_kn(dir)->files, list) {
		if (!strcmp(f->name, name)) {
			found = f;
			list_del(&f->list);
			break;
		}
	}
	path = kpi_kn_path(dir, name);
	if (path)
		rustos_kpi_sysfs_remove(path);
	mutex_unlock(&kpi_sysfs_lock);
	kfree(path);
	kfree(found);
}

/* Files of @dir; RustOS has removed (and drained) them. */
static void kpi_free_files(struct kernfs_node *dir)
{
	struct kpi_sysfs_file *f, *n;

	list_for_each_entry_safe(f, n, &to_kpi_kn(dir)->files, list) {
		list_del(&f->list);
		kfree(f);
	}
}

int sysfs_create_file_ns(struct kobject *kobj, const struct attribute *attr, const void *ns)
{
	if (WARN_ON(!kobj || !kobj->sd || !attr))
		return -EINVAL;
	return kpi_add_file(kobj->sd, kobj, attr, NULL, attr->mode);
}

int sysfs_create_files(struct kobject *kobj, const struct attribute * const *attrs)
{
	int err = 0;

	for (int i = 0; attrs[i] && !err; i++)
		err = sysfs_create_file(kobj, attrs[i]);
	return err;
}

void sysfs_remove_file_ns(struct kobject *kobj, const struct attribute *attr, const void *ns)
{
	if (kobj && kobj->sd)
		kpi_remove_file(kobj->sd, attr->name);
}

void sysfs_remove_files(struct kobject *kobj, const struct attribute * const *attrs)
{
	for (int i = 0; attrs[i]; i++)
		sysfs_remove_file(kobj, attrs[i]);
}

struct kpi_remove_self {
	struct work_struct work;
	struct kobject *kobj;
	const struct attribute *attr;
};

static void kpi_remove_self_fn(struct work_struct *work)
{
	struct kpi_remove_self *r = container_of(work, struct kpi_remove_self, work);

	sysfs_remove_file(r->kobj, r->attr);
	kobject_put(r->kobj);
	kfree(r);
}

/*
 * Called from the attribute's own store(): removing it here would wait for
 * that very call to finish, so the removal runs from a workqueue.
 */
bool sysfs_remove_file_self(struct kobject *kobj, const struct attribute *attr)
{
	struct kpi_remove_self *r = kzalloc(sizeof(*r), GFP_KERNEL);

	if (!r)
		return false;
	r->kobj = kobject_get(kobj);
	r->attr = attr;
	INIT_WORK(&r->work, kpi_remove_self_fn);
	schedule_work(&r->work);
	return true;
}

int sysfs_create_bin_file(struct kobject *kobj, const struct bin_attribute *attr)
{
	if (WARN_ON(!kobj || !kobj->sd || !attr))
		return -EINVAL;
	return kpi_add_file(kobj->sd, kobj, &attr->attr, attr, attr->attr.mode);
}

void sysfs_remove_bin_file(struct kobject *kobj, const struct bin_attribute *attr)
{
	if (kobj && kobj->sd)
		kpi_remove_file(kobj->sd, attr->attr.name);
}

int sysfs_chmod_file(struct kobject *kobj, const struct attribute *attr, umode_t mode)
{
	return 0;
}

/* ----------------------------------------------------------- directories */

int sysfs_create_dir_ns(struct kobject *kobj, const void *ns)
{
	struct kernfs_node *parent, *kn;
	char *path;
	int err;

	if (WARN_ON(!kobj))
		return -EINVAL;
	parent = kpi_kobj_dir(kobj->parent);
	kn = kpi_kn_new(parent, kobject_name(kobj), kobj);
	if (!kn)
		return -ENOMEM;
	mutex_lock(&kpi_sysfs_lock);
	path = kpi_kn_path(kn, NULL);
	err = path ? rustos_kpi_sysfs_mkdir(path) : -ENOMEM;
	if (!err) {
		parent->dir.subdirs++;
		kobj->sd = kn;
	}
	mutex_unlock(&kpi_sysfs_lock);
	if (err == -EEXIST)
		pr_warn("sysfs: cannot create duplicate filename '/sys/%s'\n", path);
	kfree(path);
	if (err)
		kernfs_put(kn);
	return err;
}

static void kpi_remove_dir(struct kernfs_node *kn)
{
	char *path;

	mutex_lock(&kpi_sysfs_lock);
	path = kpi_kn_path(kn, NULL);
	if (path)
		rustos_kpi_sysfs_remove(path);
	kpi_free_files(kn);
	if (kpi_kn_parent(kn) && kpi_kn_parent(kn)->dir.subdirs)
		kpi_kn_parent(kn)->dir.subdirs--;
	mutex_unlock(&kpi_sysfs_lock);
	kfree(path);
}

void sysfs_remove_dir(struct kobject *kobj)
{
	struct kernfs_node *kn = kobj->sd;

	if (!kn)
		return;
	kobj->sd = NULL;
	kpi_remove_dir(kn);
	kernfs_put(kn);	/* the reference the tree held */
}

static int kpi_move(struct kernfs_node *kn, struct kernfs_node *new_parent, const char *new_name)
{
	struct kernfs_node *old_parent = kpi_kn_parent(kn);
	char *old_path, *new_path, *name = NULL;
	int err;

	if (new_name) {
		name = kstrdup(new_name, GFP_KERNEL);
		if (!name)
			return -ENOMEM;
	}
	mutex_lock(&kpi_sysfs_lock);
	old_path = kpi_kn_path(kn, NULL);
	new_path = kpi_kn_path(new_parent, name ?: kpi_kn_name(kn));
	err = old_path && new_path ? rustos_kpi_sysfs_rename(old_path, new_path) : -ENOMEM;
	if (!err) {
		if (name) {
			const char *old = kpi_kn_name(kn);

			rcu_assign_pointer(kn->name, name);
			name = (char *)old;
		}
		if (new_parent != old_parent) {
			kernfs_get(new_parent);
			rcu_assign_pointer(kn->__parent, new_parent);
			new_parent->dir.subdirs++;
			old_parent->dir.subdirs--;
		}
	}
	mutex_unlock(&kpi_sysfs_lock);
	if (!err && new_parent != old_parent)
		kernfs_put(old_parent);
	kfree(name);
	kfree(old_path);
	kfree(new_path);
	return err;
}

int sysfs_rename_dir_ns(struct kobject *kobj, const char *new_name, const void *new_ns)
{
	if (!kobj->sd)
		return -EINVAL;
	return kpi_move(kobj->sd, kpi_kn_parent(kobj->sd), new_name);
}

int sysfs_move_dir_ns(struct kobject *kobj, struct kobject *new_parent_kobj,
		      const void *new_ns)
{
	if (!kobj->sd)
		return -EINVAL;
	return kpi_move(kobj->sd, kpi_kobj_dir(new_parent_kobj), NULL);
}

/* ----------------------------------------------------------------- links */

static int kpi_create_link(struct kobject *kobj, struct kobject *target, const char *name,
			   bool warn)
{
	struct kernfs_node *dir = kpi_kobj_dir(kobj);
	char *path, *tpath = NULL, *abs = NULL;
	int err = -ENOMEM;

	if (!target || !target->sd)
		return -ENOENT;
	mutex_lock(&kpi_sysfs_lock);
	path = kpi_kn_path(dir, name);
	tpath = kpi_kn_path(target->sd, NULL);
	if (tpath)
		abs = kasprintf(GFP_KERNEL, "/sys/%s", tpath);
	if (path && abs)
		err = rustos_kpi_sysfs_add_link(path, abs);
	mutex_unlock(&kpi_sysfs_lock);
	if (err == -EEXIST && warn)
		pr_warn("sysfs: cannot create duplicate filename '/sys/%s'\n", path);
	kfree(path);
	kfree(tpath);
	kfree(abs);
	return err;
}

int sysfs_create_link(struct kobject *kobj, struct kobject *target, const char *name)
{
	return kpi_create_link(kobj, target, name, true);
}

int sysfs_create_link_nowarn(struct kobject *kobj, struct kobject *target, const char *name)
{
	return kpi_create_link(kobj, target, name, false);
}

void sysfs_remove_link(struct kobject *kobj, const char *name)
{
	char *path;

	mutex_lock(&kpi_sysfs_lock);
	path = kpi_kn_path(kpi_kobj_dir(kobj), name);
	if (path)
		rustos_kpi_sysfs_remove(path);
	mutex_unlock(&kpi_sysfs_lock);
	kfree(path);
}

void sysfs_delete_link(struct kobject *dir, struct kobject *targ, const char *name)
{
	sysfs_remove_link(dir, name);
}

int sysfs_rename_link_ns(struct kobject *kobj, struct kobject *targ, const char *old_name,
			 const char *new_name, const void *new_ns)
{
	sysfs_remove_link(kobj, old_name);
	return sysfs_create_link(kobj, targ, new_name);
}

/* ---------------------------------------------------------------- groups */

static struct kernfs_node *kpi_group_dir(struct kobject *kobj, const struct attribute_group *grp)
{
	struct kernfs_node *kn;
	char *path;
	int err;

	if (!grp->name)
		return kobj->sd;
	kn = kpi_kn_new(kobj->sd, grp->name, NULL);
	if (!kn)
		return ERR_PTR(-ENOMEM);
	mutex_lock(&kpi_sysfs_lock);
	path = kpi_kn_path(kn, NULL);
	err = path ? rustos_kpi_sysfs_mkdir(path) : -ENOMEM;
	if (!err)
		kobj->sd->dir.subdirs++;
	mutex_unlock(&kpi_sysfs_lock);
	kfree(path);
	if (err) {
		kernfs_put(kn);
		return ERR_PTR(err);
	}
	return kn;
}

/* Group directory nodes live in this list until the group is removed. */
static LIST_HEAD(kpi_group_dirs);
struct kpi_group_dir {
	struct list_head list;
	struct kobject *kobj;
	const struct attribute_group *grp;
	struct kernfs_node *kn;
};

static int kpi_group_files(struct kobject *kobj, struct kernfs_node *dir,
			   const struct attribute_group *grp)
{
	int err = 0;

	if (grp->attrs) {
		for (int i = 0; grp->attrs[i] && !err; i++) {
			struct attribute *attr = grp->attrs[i];
			umode_t mode = attr->mode;

			if (grp->is_visible) {
				mode = grp->is_visible(kobj, attr, i);
				if (!mode)
					continue;
			}
			err = kpi_add_file(dir, kobj, attr, NULL, mode);
		}
	}
	if (grp->bin_attrs) {
		for (int i = 0; grp->bin_attrs[i] && !err; i++) {
			const struct bin_attribute *battr = grp->bin_attrs[i];
			umode_t mode = battr->attr.mode;

			if (grp->is_bin_visible) {
				mode = grp->is_bin_visible(kobj, battr, i);
				if (!mode)
					continue;
			}
			err = kpi_add_file(dir, kobj, &battr->attr, battr, mode);
		}
	}
	return err;
}

int sysfs_create_group(struct kobject *kobj, const struct attribute_group *grp)
{
	struct kernfs_node *dir;
	struct kpi_group_dir *g;
	int err;

	if (WARN_ON(!kobj || !kobj->sd))
		return -EINVAL;
	dir = kpi_group_dir(kobj, grp);
	if (IS_ERR(dir))
		return PTR_ERR(dir);
	err = kpi_group_files(kobj, dir, grp);
	if (grp->name) {
		g = kzalloc(sizeof(*g), GFP_KERNEL);
		if (g) {
			g->kobj = kobj;
			g->grp = grp;
			g->kn = dir;
			mutex_lock(&kpi_sysfs_lock);
			list_add(&g->list, &kpi_group_dirs);
			mutex_unlock(&kpi_sysfs_lock);
		}
	}
	if (err)
		sysfs_remove_group(kobj, grp);
	return err;
}

int sysfs_update_group(struct kobject *kobj, const struct attribute_group *grp)
{
	sysfs_remove_group(kobj, grp);
	return sysfs_create_group(kobj, grp);
}

int sysfs_create_groups(struct kobject *kobj, const struct attribute_group **groups)
{
	int err = 0, i;

	if (!groups)
		return 0;
	for (i = 0; groups[i]; i++) {
		err = sysfs_create_group(kobj, groups[i]);
		if (err) {
			while (--i >= 0)
				sysfs_remove_group(kobj, groups[i]);
			break;
		}
	}
	return err;
}

int sysfs_update_groups(struct kobject *kobj, const struct attribute_group **groups)
{
	int err = 0;

	for (int i = 0; groups && groups[i] && !err; i++)
		err = sysfs_update_group(kobj, groups[i]);
	return err;
}

void sysfs_remove_group(struct kobject *kobj, const struct attribute_group *grp)
{
	struct kpi_group_dir *g, *found = NULL;

	if (!kobj->sd)
		return;
	if (!grp->name) {
		for (int i = 0; grp->attrs && grp->attrs[i]; i++)
			kpi_remove_file(kobj->sd, grp->attrs[i]->name);
		for (int i = 0; grp->bin_attrs && grp->bin_attrs[i]; i++)
			kpi_remove_file(kobj->sd, grp->bin_attrs[i]->attr.name);
		return;
	}
	mutex_lock(&kpi_sysfs_lock);
	list_for_each_entry(g, &kpi_group_dirs, list) {
		if (g->kobj == kobj && g->grp == grp) {
			found = g;
			list_del(&g->list);
			break;
		}
	}
	mutex_unlock(&kpi_sysfs_lock);
	if (!found)
		return;
	kpi_remove_dir(found->kn);
	kernfs_put(found->kn);
	kfree(found);
}

void sysfs_remove_groups(struct kobject *kobj, const struct attribute_group **groups)
{
	for (int i = 0; groups && groups[i]; i++)
		sysfs_remove_group(kobj, groups[i]);
}

int sysfs_merge_group(struct kobject *kobj, const struct attribute_group *grp)
{
	struct kpi_group_dir *g;
	struct kernfs_node *dir = NULL;

	mutex_lock(&kpi_sysfs_lock);
	list_for_each_entry(g, &kpi_group_dirs, list)
		if (g->kobj == kobj && !strcmp(g->grp->name, grp->name))
			dir = g->kn;
	mutex_unlock(&kpi_sysfs_lock);
	return dir ? kpi_group_files(kobj, dir, grp) : -ENOENT;
}

void sysfs_unmerge_group(struct kobject *kobj, const struct attribute_group *grp)
{
	struct kpi_group_dir *g;
	struct kernfs_node *dir = NULL;

	mutex_lock(&kpi_sysfs_lock);
	list_for_each_entry(g, &kpi_group_dirs, list)
		if (g->kobj == kobj && !strcmp(g->grp->name, grp->name))
			dir = g->kn;
	mutex_unlock(&kpi_sysfs_lock);
	for (int i = 0; dir && grp->attrs && grp->attrs[i]; i++)
		kpi_remove_file(dir, grp->attrs[i]->name);
}

int sysfs_add_file_to_group(struct kobject *kobj, const struct attribute *attr,
			    const char *group)
{
	struct kpi_group_dir *g;
	struct kernfs_node *dir = group ? NULL : kobj->sd;

	mutex_lock(&kpi_sysfs_lock);
	list_for_each_entry(g, &kpi_group_dirs, list)
		if (group && g->kobj == kobj && !strcmp(g->grp->name, group))
			dir = g->kn;
	mutex_unlock(&kpi_sysfs_lock);
	return dir ? kpi_add_file(dir, kobj, attr, NULL, attr->mode) : -ENOENT;
}

void sysfs_remove_file_from_group(struct kobject *kobj, const struct attribute *attr,
				  const char *group)
{
	struct kpi_group_dir *g;
	struct kernfs_node *dir = group ? NULL : kobj->sd;

	mutex_lock(&kpi_sysfs_lock);
	list_for_each_entry(g, &kpi_group_dirs, list)
		if (group && g->kobj == kobj && !strcmp(g->grp->name, group))
			dir = g->kn;
	mutex_unlock(&kpi_sysfs_lock);
	if (dir)
		kpi_remove_file(dir, attr->name);
}

int sysfs_add_link_to_group(struct kobject *kobj, const char *group_name,
			    struct kobject *target, const char *link_name)
{
	char *name = kasprintf(GFP_KERNEL, "%s/%s", group_name, link_name);
	int err = name ? sysfs_create_link(kobj, target, name) : -ENOMEM;

	kfree(name);
	return err;
}

void sysfs_remove_link_from_group(struct kobject *kobj, const char *group_name,
				  const char *link_name)
{
	char *name = kasprintf(GFP_KERNEL, "%s/%s", group_name, link_name);

	if (name)
		sysfs_remove_link(kobj, name);
	kfree(name);
}

/* --------------------------------------------- ownership (no user ids) */

int sysfs_change_owner(struct kobject *kobj, kuid_t kuid, kgid_t kgid)
{
	return 0;
}

int sysfs_file_change_owner(struct kobject *kobj, const char *name, kuid_t kuid, kgid_t kgid)
{
	return 0;
}

int sysfs_link_change_owner(struct kobject *kobj, struct kobject *targ, const char *name,
			    kuid_t kuid, kgid_t kgid)
{
	return 0;
}

int sysfs_groups_change_owner(struct kobject *kobj, const struct attribute_group **groups,
			      kuid_t kuid, kgid_t kgid)
{
	return 0;
}

int sysfs_group_change_owner(struct kobject *kobj, const struct attribute_group *groups,
			     kuid_t kuid, kgid_t kgid)
{
	return 0;
}

void sysfs_notify(struct kobject *kobj, const char *dir, const char *attr)
{
}
