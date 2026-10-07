/* SPDX-License-Identifier: GPL-2.0-or-later */
/*
 * lib/crypto/blake2s.c's architecture hook ($(SRCARCH)/blake2s.h): the
 * kernel runs without SIMD, so the generic compression function is used.
 */
#define blake2s_compress blake2s_compress_generic
