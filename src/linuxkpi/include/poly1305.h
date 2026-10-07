/* SPDX-License-Identifier: GPL-2.0-or-later */
/*
 * lib/crypto/poly1305.c's architecture hook ($(SRCARCH)/poly1305.h): the
 * kernel runs without SIMD, so the generic 64-bit implementation
 * (poly1305-donna64.c) is used.
 */
#define poly1305_block_init poly1305_block_init_generic
#define poly1305_blocks poly1305_blocks_generic
#define poly1305_emit poly1305_emit_generic
