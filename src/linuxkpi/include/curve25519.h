/* SPDX-License-Identifier: GPL-2.0-or-later */
/*
 * lib/crypto/curve25519.c's architecture hook ($(SRCARCH)/curve25519.h):
 * the generic 64-bit implementation (curve25519-hacl64.c) is used, as
 * Linux does on CPUs without BMI2/ADX.
 */
static void curve25519_arch(u8 mypublic[CURVE25519_KEY_SIZE],
			    const u8 secret[CURVE25519_KEY_SIZE],
			    const u8 basepoint[CURVE25519_KEY_SIZE])
{
	curve25519_generic(mypublic, secret, basepoint);
}

static void curve25519_base_arch(u8 pub[CURVE25519_KEY_SIZE],
				 const u8 secret[CURVE25519_KEY_SIZE])
{
	curve25519_generic(pub, secret, curve25519_base_point);
}
