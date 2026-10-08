// SPDX-License-Identifier: GPL-2.0-or-later
/*
 * What i915 takes from x86 platform code (M40).
 *
 * Stolen memory: firmware reserves part of RAM for the integrated GPU, and
 * Linux's early PCI quirks (arch/x86/kernel/early-quirks.c) record where in
 * intel_graphics_stolen_res, which i915 then uses for its framebuffer
 * compression and GuC. The same registers are read here from the GPU at
 * 00:02.0: the size from GGC (gen8 and later encoding), the base from BDSM
 * (gen6-9) or the 64-bit BSM (gen11 and later).
 */
#include <linux/init.h>
#include <linux/ioport.h>
#include <linux/pci.h>
#include <linux/pnp.h>
#include <drm/intel/i915_drm.h>
#include "kpi.h"

struct resource intel_graphics_stolen_res __ro_after_init = DEFINE_RES_MEM(0, 0);

/* Gen11 and later integrated GPUs by device-id prefix: Ice Lake, Elkhart/
 * Jasper Lake, Tiger Lake, Rocket Lake, Alder/Raptor Lake, Meteor/Arrow
 * Lake, Lunar Lake. */
static bool kpi_gen11_plus(u16 id)
{
	static const u8 prefixes[] = { 0x8a, 0x45, 0x4e, 0x9a, 0x4c, 0x46, 0xa7, 0x7d, 0x64, 0xb0 };

	for (unsigned int i = 0; i < ARRAY_SIZE(prefixes); i++)
		if ((id >> 8) == prefixes[i])
			return true;
	return false;
}

static int __init kpi_intel_stolen_init(void)
{
	struct pci_dev *gpu = pci_get_domain_bus_and_slot(0, 0, PCI_DEVFN(2, 0));
	u16 ggc, gms;
	u64 base, size;
	u32 lo, hi;

	if (!gpu)
		return 0;
	if (gpu->vendor != PCI_VENDOR_ID_INTEL || (gpu->class >> 16) != PCI_BASE_CLASS_DISPLAY)
		goto out;
	pci_read_config_word(gpu, SNB_GMCH_CTRL, &ggc);
	gms = (ggc >> BDW_GMCH_GMS_SHIFT) & BDW_GMCH_GMS_MASK;
	size = gms < 0xf0 ? (u64)gms * SZ_32M : (u64)(gms - 0xf0) * SZ_4M + SZ_4M;
	if (kpi_gen11_plus(gpu->device)) {
		pci_read_config_dword(gpu, INTEL_GEN11_BSM_DW0, &lo);
		pci_read_config_dword(gpu, INTEL_GEN11_BSM_DW1, &hi);
		base = ((u64)hi << 32) | (lo & INTEL_BSM_MASK);
	} else {
		pci_read_config_dword(gpu, INTEL_BSM, &lo);
		base = lo & INTEL_BSM_MASK;
	}
	if (base && size) {
		intel_graphics_stolen_res = DEFINE_RES_MEM(base, size);
		pr_info("Intel graphics stolen memory: %pR\n", &intel_graphics_stolen_res);
	}
out:
	pci_dev_put(gpu);
	return 0;
}
/* After the PCI devices exist, before i915 probes (device initcalls). */
fs_initcall(kpi_intel_stolen_init);

/* intel_ips (Ironlake turbo) is not built. */
void ips_link_to_i915_driver(void)
{
}

/* PNP resources (motherboard reservations) are not tracked. */
int pnp_range_reserved(resource_size_t start, resource_size_t end)
{
	return 0;
}
