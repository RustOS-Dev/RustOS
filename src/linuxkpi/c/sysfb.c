// SPDX-License-Identifier: GPL-2.0-or-later
/*
 * The firmware framebuffer as a "simple-framebuffer" platform device, as
 * Linux's sysfb does with CONFIG_SYSFB_SIMPLEFB (drivers/firmware/
 * sysfb_simplefb.c): simpledrm binds it and becomes /dev/dri/card0 with
 * the console on it, until a display driver for the GPU takes the
 * aperture and removes it.
 */
#include <linux/ioport.h>
#include <linux/platform_data/simplefb.h>
#include <linux/platform_device.h>
#include "kpi.h"

static struct simplefb_platform_data kpi_sysfb_mode;

bool kpi_sysfb_register(void)
{
	struct platform_device *pdev;
	struct resource res = {};
	u32 bpp;
	int bgr;
	u64 len, phys = rustos_kpi_fb_phys(&len);

	if (!phys || !rustos_kpi_fb_geometry(&kpi_sysfb_mode.width, &kpi_sysfb_mode.height,
					     &kpi_sysfb_mode.stride, &bpp, &bgr))
		return false;
	switch (bpp) {
	case 4:
		/* Little-endian names: blue in the low byte is x8r8g8b8. */
		kpi_sysfb_mode.format = bgr ? "x8r8g8b8" : "x8b8g8r8";
		break;
	case 3:
		kpi_sysfb_mode.format = "r8g8b8";
		break;
	default:
		return false;
	}
	res.name = "simple-framebuffer";
	res.start = phys;
	res.end = phys + len - 1;
	res.flags = IORESOURCE_MEM;
	pdev = platform_device_register_resndata(NULL, "simple-framebuffer", 0, &res, 1,
						 &kpi_sysfb_mode, sizeof(kpi_sysfb_mode));
	if (IS_ERR(pdev)) {
		pr_warn("sysfb: simple-framebuffer: %ld\n", PTR_ERR(pdev));
		return false;
	}
	return true;
}
