// SPDX-License-Identifier: GPL-2.0-or-later
/*
 * ACPI side of the Linux I2C core, until drivers/i2c/i2c-core-acpi.c is
 * imported with the ACPI resource (I2cSerialBusV2) support it needs: no
 * I2C clients are enumerated from ACPI yet; adapters and drivers that
 * create their clients themselves (NIC sensor buses) work.
 */
#include <linux/acpi.h>
#include <linux/i2c.h>
#include <linux/notifier.h>
#include "kpi.h"

void i2c_acpi_register_devices(struct i2c_adapter *adap)
{
}

int i2c_acpi_get_irq(struct i2c_client *client, bool *wake_capable)
{
	return 0;
}

struct notifier_block i2c_acpi_notifier = {};

/* ACPI operation regions on I2C buses (GenericSerialBus): none. */
int i2c_acpi_install_space_handler(struct i2c_adapter *adapter)
{
	return 0;
}

void i2c_acpi_remove_space_handler(struct i2c_adapter *adapter)
{
}

bool i2c_acpi_waive_d0_probe(struct device *dev)
{
	return false;
}
