/* SPDX-License-Identifier: GPL-2.0-or-later */
/*
 * lib/crypto/chacha.c's architecture hook ($(SRCARCH)/chacha.h): the
 * kernel runs without SIMD, so the generic C implementation is used.
 */
#define chacha_crypt_arch chacha_crypt_generic
#define hchacha_block_arch hchacha_block_generic
