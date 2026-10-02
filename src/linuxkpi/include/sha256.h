/* SPDX-License-Identifier: GPL-2.0-or-later */
/*
 * lib/crypto/sha256.c's architecture hook ($(SRCARCH)/sha256.h): the
 * kernel runs without SIMD, so the generic block function is used.
 */
#define sha256_blocks sha256_blocks_generic
