/* SPDX-License-Identifier: GPL-2.0-or-later */
/*
 * Offsets into RustOS's per-CPU block (src/arch/x86_64/cpu.rs `PerCpu`),
 * which the GS base points at. src/arch/x86_64/cpu.rs asserts that these
 * match the Rust layout.
 */
#ifndef _RUSTOS_PERCPU_LAYOUT_H
#define _RUSTOS_PERCPU_LAYOUT_H

#define RUSTOS_PERCPU_CPU_ID		24	/* u32 */
#define RUSTOS_PERCPU_PREEMPT_COUNT	56	/* u32, shared with RustOS */
#define RUSTOS_PERCPU_NEED_RESCHED	60	/* u32 */
#define RUSTOS_PERCPU_LINUX_OFFSET	72	/* u64: this CPU's per-CPU offset */

#endif
