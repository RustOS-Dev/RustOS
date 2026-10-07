// SPDX-License-Identifier: GPL-2.0-or-later
/*
 * Built with FPU flags (as Linux builds amdgpu's display core with
 * CC_FLAGS_FPU): floating-point code the self-test runs between
 * kernel_fpu_begin() and kernel_fpu_end().
 */
#include <linux/types.h>

int kpi_fpu_compute(int n);

/* Integer part of 1000 * sum(1/k^2, k = 1..n), via doubles. */
int kpi_fpu_compute(int n)
{
	double sum = 0;

	for (int k = 1; k <= n; k++)
		sum += 1.0 / ((double)k * k);
	return (int)(sum * 1000.0);
}
