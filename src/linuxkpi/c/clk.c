// SPDX-License-Identifier: GPL-2.0-or-later
/*
 * Clocks, in place of the common clock framework: a device's clock is a
 * fixed rate known from its ACPI id. AMD's ACPI I2C and UART controllers
 * get the rates Linux's acpi_apd.c registers for them; other devices
 * have no clock (clk_get() fails, the _optional variants give NULL, which
 * every clk_* call accepts).
 */
#include <linux/acpi.h>
#include <linux/clk.h>
#include <linux/clk-provider.h>
#include <linux/device.h>
#include <linux/slab.h>
#include "kpi.h"

struct clk {
	unsigned long rate;
	unsigned int enable_count;
};

static const struct acpi_device_id kpi_clk_rates[] = {
	{ "AMD0010", 133000000 },	/* Carrizo I2C */
	{ "AMDI0010", 133000000 },	/* I2C (Raven and later) */
	{ "AMDI0019", 133000000 },
	{ "AMDI0510", 150000000 },	/* Wildcat I2C */
	{ "AMD0020", 48000000 },	/* UART */
	{ "AMDI0020", 48000000 },
	{ "AMDI0022", 48000000 },
	{ }
};

struct clk *clk_get(struct device *dev, const char *con_id)
{
	const struct acpi_device_id *id;
	struct clk *clk;

	if (!dev || con_id)
		return ERR_PTR(-ENOENT);
	id = acpi_match_device(kpi_clk_rates, dev);
	if (!id)
		return ERR_PTR(-ENOENT);
	clk = kzalloc(sizeof(*clk), GFP_KERNEL);
	if (!clk)
		return ERR_PTR(-ENOMEM);
	clk->rate = id->driver_data;
	return clk;
}

void clk_put(struct clk *clk)
{
	if (!IS_ERR_OR_NULL(clk))
		kfree(clk);
}

static void kpi_devm_clk_release(struct device *dev, void *res)
{
	struct clk *clk = *(struct clk **)res;

	if (clk && clk->enable_count)
		clk->enable_count = 0;
	clk_put(clk);
}

static struct clk *kpi_devm_clk(struct device *dev, const char *id, bool optional,
				bool enable)
{
	struct clk **dr = devres_alloc(kpi_devm_clk_release, sizeof(*dr), GFP_KERNEL);
	struct clk *clk;

	if (!dr)
		return ERR_PTR(-ENOMEM);
	clk = optional ? clk_get_optional(dev, id) : clk_get(dev, id);
	if (IS_ERR(clk)) {
		devres_free(dr);
		return clk;
	}
	if (enable)
		clk_prepare_enable(clk);
	*dr = clk;
	devres_add(dev, dr);
	return clk;
}

struct clk *devm_clk_get(struct device *dev, const char *id)
{
	return kpi_devm_clk(dev, id, false, false);
}

struct clk *devm_clk_get_optional(struct device *dev, const char *id)
{
	return kpi_devm_clk(dev, id, true, false);
}

struct clk *devm_clk_get_enabled(struct device *dev, const char *id)
{
	return kpi_devm_clk(dev, id, false, true);
}

struct clk *devm_clk_get_optional_enabled(struct device *dev, const char *id)
{
	return kpi_devm_clk(dev, id, true, true);
}

int clk_prepare(struct clk *clk)
{
	return 0;
}

void clk_unprepare(struct clk *clk)
{
}

int clk_enable(struct clk *clk)
{
	if (clk)
		clk->enable_count++;
	return 0;
}

void clk_disable(struct clk *clk)
{
	if (clk && clk->enable_count)
		clk->enable_count--;
}

unsigned long clk_get_rate(struct clk *clk)
{
	return clk ? clk->rate : 0;
}

int clk_set_rate(struct clk *clk, unsigned long rate)
{
	return clk ? -EINVAL : 0;
}

long clk_round_rate(struct clk *clk, unsigned long rate)
{
	return clk ? clk->rate : 0;
}

bool clk_is_enabled_when_prepared(struct clk *clk)
{
	return false;
}
