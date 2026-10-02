// SPDX-License-Identifier: GPL-2.0-or-later
/*
 * SD/MMC cards as RustOS block devices.
 *
 * Linux's MMC core and host drivers (SDHCI) find and initialise cards; this
 * driver on the MMC bus stands in for mmc_block (whose request queue is
 * blk-mq): each SD/MMC card becomes a RustOS disk (mmcblkN,
 * src/linuxkpi/mmc.rs), and reads and writes are single MMC requests
 * (CMD17/18, CMD24/25 with CMD12) through a DMA-able bounce buffer.
 */
#include <linux/gfp.h>
#include <linux/mm.h>
#include <linux/mmc/card.h>
#include <linux/mmc/core.h>
#include <linux/mmc/host.h>
#include <linux/mmc/mmc.h>
#include <linux/mmc/sd.h>
#include <linux/module.h>
#include <linux/scatterlist.h>
#include <linux/slab.h>
#include <linux/delay.h>
#include "kpi.h"
/* drivers/mmc/core (cflags in groups/mmc.list) */
#include "core.h"
#include "card.h"
#include "bus.h"
#include "mmc_ops.h"

#define KPI_MMC_BOUNCE_ORDER	4	/* 64 KiB */

struct kpi_mmc_disk {
	struct mmc_card *card;
	u64 handle;
	void *bounce;
	unsigned int max_blocks;
	struct mutex lock;
};

/* Wait for the card to leave the programming state after a write. */
static int kpi_mmc_wait_ready(struct mmc_card *card)
{
	unsigned long timeout = jiffies + msecs_to_jiffies(10000);
	u32 status;
	int err;

	do {
		err = mmc_send_status(card, &status);
		if (err)
			return err;
		if ((status & R1_READY_FOR_DATA) && R1_CURRENT_STATE(status) != R1_STATE_PRG)
			return 0;
		usleep_range(100, 200);
	} while (time_before(jiffies, timeout));
	return -ETIMEDOUT;
}

static int kpi_mmc_xfer(struct kpi_mmc_disk *d, u64 lba, unsigned int blocks, bool write)
{
	struct mmc_card *card = d->card;
	struct mmc_request mrq = {};
	struct mmc_command cmd = {}, stop = {};
	struct mmc_data data = {};
	struct scatterlist sg;

	cmd.opcode = write ? (blocks > 1 ? MMC_WRITE_MULTIPLE_BLOCK : MMC_WRITE_BLOCK)
			   : (blocks > 1 ? MMC_READ_MULTIPLE_BLOCK : MMC_READ_SINGLE_BLOCK);
	/* Standard-capacity cards take byte addresses. */
	cmd.arg = mmc_card_is_blockaddr(card) ? lba : lba << 9;
	cmd.flags = MMC_RSP_SPI_R1 | MMC_RSP_R1 | MMC_CMD_ADTC;
	data.blksz = 512;
	data.blocks = blocks;
	data.flags = write ? MMC_DATA_WRITE : MMC_DATA_READ;
	sg_init_one(&sg, d->bounce, blocks * 512);
	data.sg = &sg;
	data.sg_len = 1;
	mmc_set_data_timeout(&data, card);
	mrq.cmd = &cmd;
	mrq.data = &data;
	if (blocks > 1) {
		stop.opcode = MMC_STOP_TRANSMISSION;
		stop.flags = MMC_RSP_SPI_R1B | MMC_RSP_R1B | MMC_CMD_AC;
		mrq.stop = &stop;
	}
	mmc_wait_for_req(card->host, &mrq);
	if (cmd.error)
		return cmd.error;
	if (data.error)
		return data.error;
	if (mrq.stop && stop.error)
		return stop.error;
	if (data.bytes_xfered != blocks * 512)
		return -EIO;
	return write ? kpi_mmc_wait_ready(card) : 0;
}

/* Read or write `count` 512-byte sectors at `lba` (from RustOS). */
int kpi_mmc_rw(struct kpi_mmc_disk *d, u64 lba, u32 count, u8 *buf, int write)
{
	int err = 0;

	mutex_lock(&d->lock);
	mmc_claim_host(d->card->host);
	while (count && !err) {
		unsigned int n = min(count, d->max_blocks);

		if (write)
			memcpy(d->bounce, buf, n * 512);
		err = kpi_mmc_xfer(d, lba, n, write);
		if (!err && !write)
			memcpy(buf, d->bounce, n * 512);
		lba += n;
		buf += n * 512;
		count -= n;
	}
	mmc_release_host(d->card->host);
	mutex_unlock(&d->lock);
	return err;
}

static int kpi_mmc_probe(struct mmc_card *card)
{
	struct kpi_mmc_disk *d;
	char model[64];
	u64 sectors;

	if (!mmc_card_sd(card) && !mmc_card_mmc(card))
		return -ENODEV;
	d = kzalloc(sizeof(*d), GFP_KERNEL);
	if (!d)
		return -ENOMEM;
	d->bounce = (void *)__get_free_pages(GFP_KERNEL | GFP_DMA32, KPI_MMC_BOUNCE_ORDER);
	if (!d->bounce) {
		kfree(d);
		return -ENOMEM;
	}
	mutex_init(&d->lock);
	d->card = card;
	d->max_blocks = min3((unsigned int)(PAGE_SIZE << KPI_MMC_BOUNCE_ORDER) / 512,
			     card->host->max_blk_count, card->host->max_req_size / 512);
	if (!d->max_blocks)
		d->max_blocks = 1;
	if (mmc_card_sd(card))
		sectors = card->csd.capacity << (card->csd.read_blkbits - 9);
	else
		sectors = mmc_card_is_blockaddr(card) ? card->ext_csd.sectors
						   : card->csd.capacity << (card->csd.read_blkbits - 9);
	snprintf(model, sizeof(model), "%s %s", mmc_card_sd(card) ? "SD" : "MMC",
		 card->cid.prod_name);
	dev_set_drvdata(&card->dev, d);
	d->handle = rustos_kpi_mmc_disk_add(d, sectors, model, mmc_card_readonly(card));
	pr_info("%s: %s %s %llu MiB\n", mmc_hostname(card->host), mmc_card_sd(card) ? "SD" : "MMC",
		card->cid.prod_name, sectors >> 11);
	return 0;
}

static void kpi_mmc_remove(struct mmc_card *card)
{
	struct kpi_mmc_disk *d = dev_get_drvdata(&card->dev);

	if (!d)
		return;
	rustos_kpi_mmc_disk_remove(d->handle);
	mutex_lock(&d->lock);	/* no request in flight */
	mutex_unlock(&d->lock);
	free_pages((unsigned long)d->bounce, KPI_MMC_BOUNCE_ORDER);
	kfree(d);
}

static struct mmc_driver kpi_mmc_driver = {
	.drv = {
		.name = "mmcblk",
	},
	.probe = kpi_mmc_probe,
	.remove = kpi_mmc_remove,
};

static int __init kpi_mmc_init(void)
{
	return mmc_register_driver(&kpi_mmc_driver);
}
module_init(kpi_mmc_init);
