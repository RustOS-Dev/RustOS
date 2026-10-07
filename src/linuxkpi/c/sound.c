// SPDX-License-Identifier: GPL-2.0-or-later
/*
 * ALSA's card list for RustOS's /proc/asound/cards (src/sound/mod.rs),
 * in the format of Linux's sound/core/info.c.
 */
#include <sound/core.h>
#include "kpi.h"

int kpi_sound_cards(char *buf, int len)
{
	int n = 0;

	for (int i = 0; i < SNDRV_CARDS && n < len; i++) {
		struct snd_card *card = snd_card_ref(i);

		if (!card)
			continue;
		n += scnprintf(buf + n, len - n, "%2i [%-15s]: %s - %s\n%22s%s\n", i,
			       card->id, card->driver, card->shortname, "", card->longname);
		snd_card_unref(card);
	}
	return n;
}
