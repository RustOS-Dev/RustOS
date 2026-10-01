// SPDX-License-Identifier: GPL-2.0-or-later
/*
 * Linux puts module_init()/*_initcall() entries (32-bit offsets to the
 * function) in .initcall<level>.init sections; build/linuxkpi.rs renames
 * those to kpi_initcall_<level> so the linker provides __start_/__stop_
 * symbols, and src/linuxkpi/mod.rs runs them in level order. This file
 * gives every level one zero entry (skipped by the runner) so the
 * symbols exist even when no driver uses a level.
 */
#define KPI_INITCALL_LEVEL(lvl)							\
	asm(".section kpi_initcall_" #lvl ",\"a\"\n\t.long 0\n\t.previous")

KPI_INITCALL_LEVEL(early);
KPI_INITCALL_LEVEL(0);
KPI_INITCALL_LEVEL(0s);
KPI_INITCALL_LEVEL(1);
KPI_INITCALL_LEVEL(1s);
KPI_INITCALL_LEVEL(2);
KPI_INITCALL_LEVEL(2s);
KPI_INITCALL_LEVEL(3);
KPI_INITCALL_LEVEL(3s);
KPI_INITCALL_LEVEL(4);
KPI_INITCALL_LEVEL(4s);
KPI_INITCALL_LEVEL(5);
KPI_INITCALL_LEVEL(5s);
KPI_INITCALL_LEVEL(rootfs);
KPI_INITCALL_LEVEL(6);
KPI_INITCALL_LEVEL(6s);
KPI_INITCALL_LEVEL(7);
KPI_INITCALL_LEVEL(7s);

#define KPI_INITCALL_BOUNDS(lvl)						\
	{ __start_kpi_initcall_##lvl, __stop_kpi_initcall_##lvl }
#define KPI_INITCALL_EXTERN(lvl)						\
	extern const int __start_kpi_initcall_##lvl[], __stop_kpi_initcall_##lvl[]

KPI_INITCALL_EXTERN(early); KPI_INITCALL_EXTERN(0); KPI_INITCALL_EXTERN(0s);
KPI_INITCALL_EXTERN(1); KPI_INITCALL_EXTERN(1s); KPI_INITCALL_EXTERN(2);
KPI_INITCALL_EXTERN(2s); KPI_INITCALL_EXTERN(3); KPI_INITCALL_EXTERN(3s);
KPI_INITCALL_EXTERN(4); KPI_INITCALL_EXTERN(4s); KPI_INITCALL_EXTERN(5);
KPI_INITCALL_EXTERN(5s); KPI_INITCALL_EXTERN(rootfs); KPI_INITCALL_EXTERN(6);
KPI_INITCALL_EXTERN(6s); KPI_INITCALL_EXTERN(7); KPI_INITCALL_EXTERN(7s);

static const struct { const int *start, *stop; } kpi_initcall_levels[] = {
	KPI_INITCALL_BOUNDS(early), KPI_INITCALL_BOUNDS(0), KPI_INITCALL_BOUNDS(0s),
	KPI_INITCALL_BOUNDS(1), KPI_INITCALL_BOUNDS(1s), KPI_INITCALL_BOUNDS(2),
	KPI_INITCALL_BOUNDS(2s), KPI_INITCALL_BOUNDS(3), KPI_INITCALL_BOUNDS(3s),
	KPI_INITCALL_BOUNDS(4), KPI_INITCALL_BOUNDS(4s), KPI_INITCALL_BOUNDS(5),
	KPI_INITCALL_BOUNDS(5s), KPI_INITCALL_BOUNDS(rootfs), KPI_INITCALL_BOUNDS(6),
	KPI_INITCALL_BOUNDS(6s), KPI_INITCALL_BOUNDS(7), KPI_INITCALL_BOUNDS(7s),
};

/* Level `level` in run order: 0 on success, -1 past the last level. */
int kpi_initcall_bounds(int level, const int **start, const int **stop)
{
	if (level < 0 || level >= (int)(sizeof(kpi_initcall_levels) / sizeof(kpi_initcall_levels[0])))
		return -1;
	*start = kpi_initcall_levels[level].start;
	*stop = kpi_initcall_levels[level].stop;
	return 0;
}
