// SPDX-License-Identifier: GPL-2.0-or-later
/*
 * GPIO, pin control and DRM panel calls for builds without gpiolib
 * (linux-platform) or DRM (linux-drm). The device core and drivers in
 * other groups call into these APIs, which then find no GPIOs or panels,
 * as on a system without them. When the subsystem is built, its
 * definitions replace these weak ones.
 */
#include <linux/acpi.h>
#include <linux/device.h>
#include <linux/err.h>
#include <linux/gpio/consumer.h>
#include <linux/gpio/machine.h>
#include <linux/pinctrl/consumer.h>
#include <linux/pinctrl/devinfo.h>
#include <drm/drm_panel.h>
#include "kpi.h"

/* drivers/base/pinctrl.c: devices have no pin control states (ACPI
 * firmware sets pins up). */
int __weak pinctrl_bind_pins(struct device *dev)
{
	return 0;
}

/* gpiolib-swnode.c is not built: software nodes name no GPIOs. */
struct gpio_desc *swnode_find_gpio(struct fwnode_handle *fwnode, const char *con_id,
				   unsigned int idx, unsigned long *flags)
{
	return ERR_PTR(-ENOENT);
}

int __weak pinctrl_init_done(struct device *dev)
{
	return 0;
}

/* ---------------------------------- gpiolib consumer API, without gpiolib */

int __weak acpi_dev_gpio_irq_wake_get_by(struct acpi_device *adev, const char *con_id,
					 int index, bool *wake_capable)
{
	return -ENOENT;
}

struct gpio_desc *__weak devm_gpiod_get_index(struct device *dev, const char *con_id,
					      unsigned int idx, enum gpiod_flags flags)
{
	return ERR_PTR(-ENOENT);
}

void __weak gpiod_add_lookup_table(struct gpiod_lookup_table *table)
{
}

void __weak gpiod_remove_lookup_table(struct gpiod_lookup_table *table)
{
}

/* No descriptor is ever handed out, so these are never reached. */
int __weak gpiod_cansleep(const struct gpio_desc *desc)
{
	return 0;
}

int __weak gpiod_get_value(const struct gpio_desc *desc)
{
	return -EINVAL;
}

int __weak gpiod_get_value_cansleep(const struct gpio_desc *desc)
{
	return -EINVAL;
}

int __weak gpiod_is_active_low(const struct gpio_desc *desc)
{
	return 0;
}

int __weak gpiod_set_config(struct gpio_desc *desc, unsigned long config)
{
	return -ENOTSUPP;
}

int __weak gpiod_set_consumer_name(struct gpio_desc *desc, const char *name)
{
	return -EINVAL;
}

int __weak gpiod_set_debounce(struct gpio_desc *desc, unsigned int debounce)
{
	return -ENOTSUPP;
}

int __weak gpiod_to_irq(const struct gpio_desc *desc)
{
	return -EINVAL;
}

void __weak gpiod_toggle_active_low(struct gpio_desc *desc)
{
}

struct gpio_desc *__weak devm_gpiod_get_optional(struct device *dev, const char *con_id,
						 enum gpiod_flags flags)
{
	return NULL;
}

/* ------------------------------------- DRM panel followers, without DRM */

bool __weak drm_is_panel_follower(struct device *dev)
{
	return false;
}

int __weak drm_panel_add_follower(struct device *follower_dev,
				  struct drm_panel_follower *follower)
{
	return -ENODEV;
}

void __weak drm_panel_remove_follower(struct drm_panel_follower *follower)
{
}

struct gpio_desc *__weak devm_gpiod_get(struct device *dev, const char *con_id,
					enum gpiod_flags flags)
{
	return ERR_PTR(-ENOENT);
}

int __weak gpiod_get_direction(struct gpio_desc *desc)
{
	return -EINVAL;
}

int __weak gpiod_direction_output(struct gpio_desc *desc, int value)
{
	return -EINVAL;
}

int __weak gpiod_set_value_cansleep(struct gpio_desc *desc, int value)
{
	return -EINVAL;
}

struct pinctrl_state *__weak pinctrl_lookup_state(struct pinctrl *p, const char *name)
{
	return ERR_PTR(-ENODEV);
}

int __weak pinctrl_select_state(struct pinctrl *p, struct pinctrl_state *s)
{
	return 0;
}
