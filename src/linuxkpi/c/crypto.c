// SPDX-License-Identifier: GPL-2.0-or-later
/*
 * LinuxKPI crypto: the crypto API subset 802.11 uses, on the library
 * ciphers in lib/crypto. Linux's crypto/ framework (templates, the
 * algorithm registry, async requests) is not compiled; each supported
 * algorithm here is a fixed table entry, and every request completes
 * synchronously.
 *
 *   aead      ccm(aes), gcm(aes)        CCMP, GCMP, BIP-GMAC
 *   shash     cmac(aes)                 BIP-CMAC, FILS
 *   skcipher  ctr(aes)                  FILS (AES-SIV)
 *
 * Scatterlists are copied into a linear buffer, processed and copied back,
 * which is fine for frame-sized data.
 */
#include <crypto/aead.h>
#include <crypto/aes.h>
#include <crypto/gcm.h>
#include <crypto/hash.h>
#include <crypto/internal/aead.h>
#include <crypto/internal/hash.h>
#include <crypto/internal/skcipher.h>
#include <crypto/skcipher.h>
#include <crypto/utils.h>
#include <linux/err.h>
#include <linux/scatterlist.h>
#include <linux/slab.h>
#include <linux/string.h>

/* ------------------------------------------------------------- helpers */

static int kpi_sg_read(struct scatterlist *sg, void *buf, size_t len, size_t skip)
{
	return sg_pcopy_to_buffer(sg, sg_nents(sg), buf, len, skip) == len ? 0 : -EINVAL;
}

static int kpi_sg_write(struct scatterlist *sg, const void *buf, size_t len, size_t skip)
{
	return sg_pcopy_from_buffer(sg, sg_nents(sg), buf, len, skip) == len ? 0 : -EINVAL;
}

/* Increment the big-endian counter in the last n bytes of block. */
static void kpi_ctr_inc(u8 *block, int n)
{
	for (int i = AES_BLOCK_SIZE - 1; i >= AES_BLOCK_SIZE - n; i--)
		if (++block[i])
			break;
}

/* XOR data with the keystream from counter block ctr (updated). */
static void kpi_aes_ctr(const struct crypto_aes_ctx *aes, u8 *ctr, int ctr_bytes, u8 *data,
			size_t len)
{
	u8 ks[AES_BLOCK_SIZE];

	while (len) {
		size_t n = min_t(size_t, len, AES_BLOCK_SIZE);

		aes_encrypt(aes, ks, ctr);
		crypto_xor(data, ks, n);
		kpi_ctr_inc(ctr, ctr_bytes);
		data += n;
		len -= n;
	}
	memzero_explicit(ks, sizeof(ks));
}

/* ------------------------------------------------------------------ AEAD */

struct kpi_aead_ctx {
	struct crypto_aes_ctx aes;	/* ccm */
	struct aesgcm_ctx gcm;		/* gcm */
	u8 key[AES_MAX_KEY_SIZE];
	unsigned int keylen;
};

static struct kpi_aead_ctx *kpi_aead_ctx(struct crypto_aead *tfm)
{
	return crypto_aead_ctx(tfm);
}

/* CCM (RFC 3610) as Linux's ccm template takes it: iv[0] = L - 1, then the
 * nonce; the last L bytes of the IV are the counter. */
static void kpi_ccm_mac(const struct crypto_aes_ctx *aes, const u8 *iv, const u8 *assoc,
			unsigned int alen, const u8 *msg, unsigned int mlen,
			unsigned int authsize, u8 *mac)
{
	unsigned int l = iv[0] + 1;
	u8 x[AES_BLOCK_SIZE], b[AES_BLOCK_SIZE];
	unsigned int pos;

	memcpy(b, iv, AES_BLOCK_SIZE);
	b[0] |= (alen ? 0x40 : 0) | (((authsize - 2) / 2) << 3);
	for (unsigned int i = 0, v = mlen; i < l; i++, v >>= 8)
		b[AES_BLOCK_SIZE - 1 - i] = v & 0xff;
	aes_encrypt(aes, x, b);
	if (alen) {
		/* Length prefix (alen < 0xff00 for every 802.11 user), then the
		 * associated data, zero padded. */
		memset(b, 0, sizeof(b));
		b[0] = alen >> 8;
		b[1] = alen;
		pos = 2;
		for (unsigned int i = 0; i < alen; i++) {
			b[pos++] = assoc[i];
			if (pos == AES_BLOCK_SIZE) {
				crypto_xor(x, b, AES_BLOCK_SIZE);
				aes_encrypt(aes, x, x);
				memset(b, 0, sizeof(b));
				pos = 0;
			}
		}
		if (pos) {
			crypto_xor(x, b, AES_BLOCK_SIZE);
			aes_encrypt(aes, x, x);
		}
	}
	for (unsigned int off = 0; off < mlen; off += AES_BLOCK_SIZE) {
		unsigned int n = min(mlen - off, (unsigned int)AES_BLOCK_SIZE);

		crypto_xor(x, msg + off, n);
		aes_encrypt(aes, x, x);
	}
	memcpy(mac, x, authsize);
}

static int kpi_ccm_crypt(struct aead_request *req, bool enc)
{
	struct crypto_aead *tfm = crypto_aead_reqtfm(req);
	struct kpi_aead_ctx *ctx = kpi_aead_ctx(tfm);
	unsigned int as = crypto_aead_authsize(tfm), alen = req->assoclen;
	unsigned int mlen = enc ? req->cryptlen : req->cryptlen - as;
	u8 ctr[AES_BLOCK_SIZE], s0[AES_BLOCK_SIZE], mac[AES_BLOCK_SIZE];
	unsigned int l = req->iv[0] + 1;
	u8 *buf;
	int err;

	if (l < 2 || l > 8 || (!enc && req->cryptlen < as))
		return -EINVAL;
	buf = kmalloc(alen + mlen + as, GFP_ATOMIC);
	if (!buf)
		return -ENOMEM;
	err = kpi_sg_read(req->src, buf, alen + mlen + (enc ? 0 : as), 0);
	if (err)
		goto out;
	memcpy(ctr, req->iv, AES_BLOCK_SIZE);
	memset(ctr + AES_BLOCK_SIZE - l, 0, l);
	aes_encrypt(&ctx->aes, s0, ctr);
	kpi_ctr_inc(ctr, l);
	if (enc) {
		kpi_ccm_mac(&ctx->aes, req->iv, buf, alen, buf + alen, mlen, as, mac);
		kpi_aes_ctr(&ctx->aes, ctr, l, buf + alen, mlen);
		crypto_xor_cpy(buf + alen + mlen, mac, s0, as);
		err = kpi_sg_write(req->dst, buf + alen, mlen + as, alen);
	} else {
		kpi_aes_ctr(&ctx->aes, ctr, l, buf + alen, mlen);
		kpi_ccm_mac(&ctx->aes, req->iv, buf, alen, buf + alen, mlen, as, mac);
		crypto_xor(mac, s0, as);
		if (crypto_memneq(mac, buf + alen + mlen, as))
			err = -EBADMSG;
		else
			err = kpi_sg_write(req->dst, buf + alen, mlen, alen);
	}
out:
	kfree_sensitive(buf);
	return err;
}

static int kpi_ccm_encrypt(struct aead_request *req)
{
	return kpi_ccm_crypt(req, true);
}

static int kpi_ccm_decrypt(struct aead_request *req)
{
	return kpi_ccm_crypt(req, false);
}

static int kpi_ccm_setkey(struct crypto_aead *tfm, const u8 *key, unsigned int keylen)
{
	return aes_expandkey(&kpi_aead_ctx(tfm)->aes, key, keylen);
}

static int kpi_ccm_setauthsize(struct crypto_aead *tfm, unsigned int authsize)
{
	return authsize >= 4 && authsize <= 16 && !(authsize & 1) ? 0 : -EINVAL;
}

/* GCM on lib/crypto/aesgcm.c, which bakes the tag size into its key. */
static int kpi_gcm_rekey(struct crypto_aead *tfm, unsigned int authsize)
{
	struct kpi_aead_ctx *ctx = kpi_aead_ctx(tfm);

	if (!ctx->keylen)
		return 0;
	return aesgcm_expandkey(&ctx->gcm, ctx->key, ctx->keylen, authsize);
}

static int kpi_gcm_setkey(struct crypto_aead *tfm, const u8 *key, unsigned int keylen)
{
	struct kpi_aead_ctx *ctx = kpi_aead_ctx(tfm);

	if (keylen != AES_KEYSIZE_128 && keylen != AES_KEYSIZE_192 && keylen != AES_KEYSIZE_256)
		return -EINVAL;
	memcpy(ctx->key, key, keylen);
	ctx->keylen = keylen;
	return kpi_gcm_rekey(tfm, crypto_aead_authsize(tfm));
}

static int kpi_gcm_setauthsize(struct crypto_aead *tfm, unsigned int authsize)
{
	int err = crypto_gcm_check_authsize(authsize);

	return err ?: kpi_gcm_rekey(tfm, authsize);
}

static int kpi_gcm_crypt(struct aead_request *req, bool enc)
{
	struct crypto_aead *tfm = crypto_aead_reqtfm(req);
	struct kpi_aead_ctx *ctx = kpi_aead_ctx(tfm);
	unsigned int as = crypto_aead_authsize(tfm), alen = req->assoclen;
	unsigned int mlen = enc ? req->cryptlen : req->cryptlen - as;
	u8 *buf;
	int err;

	if (!ctx->keylen || (!enc && req->cryptlen < as))
		return -EINVAL;
	buf = kmalloc(alen + mlen + as, GFP_ATOMIC);
	if (!buf)
		return -ENOMEM;
	err = kpi_sg_read(req->src, buf, alen + mlen + (enc ? 0 : as), 0);
	if (err)
		goto out;
	if (enc) {
		aesgcm_encrypt(&ctx->gcm, buf + alen, buf + alen, mlen, buf, alen, req->iv,
			       buf + alen + mlen);
		err = kpi_sg_write(req->dst, buf + alen, mlen + as, alen);
	} else if (!aesgcm_decrypt(&ctx->gcm, buf + alen, buf + alen, mlen, buf, alen, req->iv,
				   buf + alen + mlen)) {
		err = -EBADMSG;
	} else {
		err = kpi_sg_write(req->dst, buf + alen, mlen, alen);
	}
out:
	kfree_sensitive(buf);
	return err;
}

static int kpi_gcm_encrypt(struct aead_request *req)
{
	return kpi_gcm_crypt(req, true);
}

static int kpi_gcm_decrypt(struct aead_request *req)
{
	return kpi_gcm_crypt(req, false);
}

static struct aead_alg kpi_aead_algs[] = {
	{
		.setkey = kpi_ccm_setkey,
		.setauthsize = kpi_ccm_setauthsize,
		.encrypt = kpi_ccm_encrypt,
		.decrypt = kpi_ccm_decrypt,
		.ivsize = AES_BLOCK_SIZE,
		.maxauthsize = 16,
		.base = {
			.cra_name = "ccm(aes)",
			.cra_driver_name = "ccm-aes-rustos",
			.cra_blocksize = 1,
			.cra_ctxsize = sizeof(struct kpi_aead_ctx),
		},
	},
	{
		.setkey = kpi_gcm_setkey,
		.setauthsize = kpi_gcm_setauthsize,
		.encrypt = kpi_gcm_encrypt,
		.decrypt = kpi_gcm_decrypt,
		.ivsize = GCM_AES_IV_SIZE,
		.maxauthsize = 16,
		.base = {
			.cra_name = "gcm(aes)",
			.cra_driver_name = "gcm-aes-rustos",
			.cra_blocksize = 1,
			.cra_ctxsize = sizeof(struct kpi_aead_ctx),
		},
	},
};

static void *kpi_tfm_alloc(size_t head, struct crypto_tfm *(*base)(void *),
			   struct crypto_alg *alg)
{
	void *mem = kzalloc(head + alg->cra_ctxsize, GFP_KERNEL);
	struct crypto_tfm *tfm;

	if (!mem)
		return ERR_PTR(-ENOMEM);
	tfm = base(mem);
	refcount_set(&tfm->refcnt, 1);
	tfm->node = NUMA_NO_NODE;
	tfm->__crt_alg = alg;
	return mem;
}

static struct crypto_tfm *kpi_aead_base(void *mem)
{
	return crypto_aead_tfm(mem);
}

struct crypto_aead *crypto_alloc_aead(const char *alg_name, u32 type, u32 mask)
{
	for (int i = 0; i < ARRAY_SIZE(kpi_aead_algs); i++) {
		struct aead_alg *alg = &kpi_aead_algs[i];
		struct crypto_aead *tfm;

		if (strcmp(alg->base.cra_name, alg_name))
			continue;
		tfm = kpi_tfm_alloc(sizeof(*tfm), kpi_aead_base, &alg->base);
		if (!IS_ERR(tfm))
			tfm->authsize = alg->maxauthsize;
		return tfm;
	}
	return ERR_PTR(-ENOENT);
}

int crypto_aead_setkey(struct crypto_aead *tfm, const u8 *key, unsigned int keylen)
{
	return crypto_aead_alg(tfm)->setkey(tfm, key, keylen);
}

int crypto_aead_setauthsize(struct crypto_aead *tfm, unsigned int authsize)
{
	int err;

	if (authsize > crypto_aead_maxauthsize(tfm))
		return -EINVAL;
	err = crypto_aead_alg(tfm)->setauthsize(tfm, authsize);
	if (!err)
		tfm->authsize = authsize;
	return err;
}

int crypto_aead_encrypt(struct aead_request *req)
{
	return crypto_aead_alg(crypto_aead_reqtfm(req))->encrypt(req);
}

int crypto_aead_decrypt(struct aead_request *req)
{
	return crypto_aead_alg(crypto_aead_reqtfm(req))->decrypt(req);
}

/* ----------------------------------------------------------- cmac(aes) */

struct kpi_cmac_ctx {
	struct crypto_aes_ctx aes;
	u8 k1[AES_BLOCK_SIZE], k2[AES_BLOCK_SIZE];
};

struct kpi_cmac_state {
	u8 x[AES_BLOCK_SIZE];
	u8 buf[AES_BLOCK_SIZE];
	unsigned int len;
};

static void kpi_cmac_dbl(u8 *out, const u8 *in)
{
	u8 carry = in[0] >> 7;

	for (int i = 0; i < AES_BLOCK_SIZE - 1; i++)
		out[i] = (in[i] << 1) | (in[i + 1] >> 7);
	out[AES_BLOCK_SIZE - 1] = (in[AES_BLOCK_SIZE - 1] << 1) ^ (carry ? 0x87 : 0);
}

static int kpi_cmac_setkey(struct crypto_shash *tfm, const u8 *key, unsigned int keylen)
{
	struct kpi_cmac_ctx *ctx = crypto_shash_ctx(tfm);
	u8 l[AES_BLOCK_SIZE] = {};
	int err = aes_expandkey(&ctx->aes, key, keylen);

	if (err)
		return err;
	aes_encrypt(&ctx->aes, l, l);
	kpi_cmac_dbl(ctx->k1, l);
	kpi_cmac_dbl(ctx->k2, ctx->k1);
	memzero_explicit(l, sizeof(l));
	return 0;
}

static int kpi_cmac_init(struct shash_desc *desc)
{
	memset(shash_desc_ctx(desc), 0, sizeof(struct kpi_cmac_state));
	return 0;
}

static int kpi_cmac_finup(struct shash_desc *desc, const u8 *data, unsigned int len, u8 *out)
{
	struct kpi_cmac_ctx *ctx = crypto_shash_ctx(desc->tfm);
	struct kpi_cmac_state *st = shash_desc_ctx(desc);

	while (len) {
		unsigned int n;

		/* Keep the last block for the final step. */
		if (st->len == AES_BLOCK_SIZE) {
			crypto_xor(st->x, st->buf, AES_BLOCK_SIZE);
			aes_encrypt(&ctx->aes, st->x, st->x);
			st->len = 0;
		}
		n = min(len, AES_BLOCK_SIZE - st->len);
		memcpy(st->buf + st->len, data, n);
		st->len += n;
		data += n;
		len -= n;
	}
	if (!out)
		return 0;
	if (st->len == AES_BLOCK_SIZE) {
		crypto_xor(st->buf, ctx->k1, AES_BLOCK_SIZE);
	} else {
		st->buf[st->len] = 0x80;
		memset(st->buf + st->len + 1, 0, AES_BLOCK_SIZE - st->len - 1);
		crypto_xor(st->buf, ctx->k2, AES_BLOCK_SIZE);
	}
	crypto_xor(st->x, st->buf, AES_BLOCK_SIZE);
	aes_encrypt(&ctx->aes, out, st->x);
	memzero_explicit(st, sizeof(*st));
	return 0;
}

static struct shash_alg kpi_shash_algs[] = {
	{
		.init = kpi_cmac_init,
		.finup = kpi_cmac_finup,
		.setkey = kpi_cmac_setkey,
		.descsize = sizeof(struct kpi_cmac_state),
		.digestsize = AES_BLOCK_SIZE,
		.base = {
			.cra_name = "cmac(aes)",
			.cra_driver_name = "cmac-aes-rustos",
			.cra_blocksize = AES_BLOCK_SIZE,
			.cra_ctxsize = sizeof(struct kpi_cmac_ctx),
		},
	},
};

static struct crypto_tfm *kpi_shash_base(void *mem)
{
	return crypto_shash_tfm(mem);
}

struct crypto_shash *crypto_alloc_shash(const char *alg_name, u32 type, u32 mask)
{
	for (int i = 0; i < ARRAY_SIZE(kpi_shash_algs); i++)
		if (!strcmp(kpi_shash_algs[i].base.cra_name, alg_name))
			return kpi_tfm_alloc(sizeof(struct crypto_shash), kpi_shash_base,
					     &kpi_shash_algs[i].base);
	return ERR_PTR(-ENOENT);
}

int crypto_shash_setkey(struct crypto_shash *tfm, const u8 *key, unsigned int keylen)
{
	return crypto_shash_alg(tfm)->setkey(tfm, key, keylen);
}

int crypto_shash_init(struct shash_desc *desc)
{
	return crypto_shash_alg(desc->tfm)->init(desc);
}

int crypto_shash_finup(struct shash_desc *desc, const u8 *data, unsigned int len, u8 *out)
{
	return crypto_shash_alg(desc->tfm)->finup(desc, data, len, out);
}

int crypto_shash_digest(struct shash_desc *desc, const u8 *data, unsigned int len, u8 *out)
{
	return crypto_shash_init(desc) ?: crypto_shash_finup(desc, data, len, out);
}

/* ------------------------------------------------------------ ctr(aes) */

static int kpi_ctr_setkey(struct crypto_skcipher *tfm, const u8 *key, unsigned int keylen)
{
	return aes_expandkey(crypto_skcipher_ctx(tfm), key, keylen);
}

static int kpi_ctr_crypt(struct skcipher_request *req)
{
	struct crypto_aes_ctx *aes = crypto_skcipher_ctx(crypto_skcipher_reqtfm(req));
	u8 *buf = kmalloc(req->cryptlen, GFP_ATOMIC);
	int err;

	if (!buf)
		return -ENOMEM;
	err = kpi_sg_read(req->src, buf, req->cryptlen, 0);
	if (!err) {
		kpi_aes_ctr(aes, req->iv, AES_BLOCK_SIZE, buf, req->cryptlen);
		err = kpi_sg_write(req->dst, buf, req->cryptlen, 0);
	}
	kfree_sensitive(buf);
	return err;
}

static struct skcipher_alg kpi_skcipher_algs[] = {
	{
		.setkey = kpi_ctr_setkey,
		.encrypt = kpi_ctr_crypt,
		.decrypt = kpi_ctr_crypt,
		.min_keysize = AES_MIN_KEY_SIZE,
		.max_keysize = AES_MAX_KEY_SIZE,
		.ivsize = AES_BLOCK_SIZE,
		.chunksize = AES_BLOCK_SIZE,
		.base = {
			.cra_name = "ctr(aes)",
			.cra_driver_name = "ctr-aes-rustos",
			.cra_blocksize = 1,
			.cra_ctxsize = sizeof(struct crypto_aes_ctx),
		},
	},
};

static struct crypto_tfm *kpi_skcipher_base(void *mem)
{
	return crypto_skcipher_tfm(mem);
}

struct crypto_skcipher *crypto_alloc_skcipher(const char *alg_name, u32 type, u32 mask)
{
	for (int i = 0; i < ARRAY_SIZE(kpi_skcipher_algs); i++)
		if (!strcmp(kpi_skcipher_algs[i].base.cra_name, alg_name))
			return kpi_tfm_alloc(sizeof(struct crypto_skcipher), kpi_skcipher_base,
					     &kpi_skcipher_algs[i].base);
	return ERR_PTR(-ENOENT);
}

int crypto_skcipher_setkey(struct crypto_skcipher *tfm, const u8 *key, unsigned int keylen)
{
	return crypto_skcipher_alg(tfm)->setkey(tfm, key, keylen);
}

int crypto_skcipher_encrypt(struct skcipher_request *req)
{
	return crypto_skcipher_alg(crypto_skcipher_reqtfm(req))->encrypt(req);
}

int crypto_skcipher_decrypt(struct skcipher_request *req)
{
	return crypto_skcipher_alg(crypto_skcipher_reqtfm(req))->decrypt(req);
}

/* ----------------------------------------------------------------- common */

void crypto_destroy_tfm(void *mem, struct crypto_tfm *tfm)
{
	if (IS_ERR_OR_NULL(mem))
		return;
	if (!refcount_dec_and_test(&tfm->refcnt))
		return;
	kfree_sensitive(mem);
}
