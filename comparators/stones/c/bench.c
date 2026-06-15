/*
 * Cross-language comparator — C / OpenSSL libcrypto.
 *
 * Same methodology as the Rust + Go harnesses:
 * per (op, size): ~100 ms warmup, then 5 batches of
 * >= 10 ms each; report the best (minimum) ns/op.
 *
 * Output: CSV lines  impl,op,size,ns_per_op,gib_s
 *
 * Build (macOS / Homebrew OpenSSL 3):
 *   clang -O3 -mcpu=native -Wno-deprecated-declarations \
 *     -I$(brew --prefix openssl@3)/include \
 *     -L$(brew --prefix openssl@3)/lib \
 *     bench.c -lcrypto -o bench
 *
 * SHA1() direct (not EVP) — the lowest-overhead one-shot
 * OpenSSL offers; deprecation warning suppressed because
 * we *want* their fastest path as the comparison target.
 */

#include <openssl/sha.h>
#include <openssl/evp.h>
#include <stdio.h>
#include <stdint.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>

static double now_ns(void) {
    struct timespec ts;
    clock_gettime(CLOCK_MONOTONIC, &ts);
    return (double)ts.tv_sec * 1e9 + (double)ts.tv_nsec;
}

/* Global sink defeats dead-code elimination. */
static volatile uint8_t g_sink;

typedef void (*op_fn)(const uint8_t *in, size_t n, uint8_t *out, size_t out_cap);

static void op_sha1(const uint8_t *in, size_t n, uint8_t *out, size_t out_cap) {
    (void)out_cap;
    SHA1(in, n, out);
}

static void op_b64_encode(const uint8_t *in, size_t n, uint8_t *out, size_t out_cap) {
    (void)out_cap;
    EVP_EncodeBlock((unsigned char *)out, in, (int)n);
}

static void op_b64_decode(const uint8_t *in, size_t n, uint8_t *out, size_t out_cap) {
    (void)out_cap;
    EVP_DecodeBlock((unsigned char *)out, in, (int)n);
}

static void run_one(const char *op_name, op_fn fn, const uint8_t *in, size_t n,
                    uint8_t *out, size_t out_cap, size_t reported_size) {
    /* Warmup ~100 ms. */
    double t0 = now_ns();
    while (now_ns() - t0 < 1e8) {
        fn(in, n, out, out_cap);
        g_sink = out[0];
    }

    /* Pick K so one batch is >= ~10 ms. */
    t0 = now_ns();
    fn(in, n, out, out_cap);
    double once = now_ns() - t0;
    size_t k = once > 0 ? (size_t)(1e7 / once) + 1 : 1024;

    double best = 1e30;
    for (int b = 0; b < 5; b++) {
        t0 = now_ns();
        for (size_t i = 0; i < k; i++) {
            fn(in, n, out, out_cap);
            g_sink = out[0];
        }
        double per = (now_ns() - t0) / (double)k;
        if (per < best) best = per;
    }

    double gib_s = ((double)reported_size / 1073741824.0) / (best / 1e9);
    printf("openssl-c,%s,%zu,%.1f,%.4f\n", op_name, reported_size, best, gib_s);
}

int main(void) {
    static const size_t sizes[] = {1024, 65536, 1048576};
    for (int s = 0; s < 3; s++) {
        size_t n = sizes[s];

        uint8_t *buf = malloc(n);
        for (size_t i = 0; i < n; i++) buf[i] = (uint8_t)(i * 7);

        /* SHA-1 */
        uint8_t digest[SHA_DIGEST_LENGTH];
        run_one("sha1", op_sha1, buf, n, digest, sizeof digest, n);

        /* base64 encode: n input bytes. */
        size_t enc_cap = ((n + 2) / 3) * 4 + 1;
        uint8_t *enc = malloc(enc_cap);
        run_one("b64-encode", op_b64_encode, buf, n, enc, enc_cap, n);

        /* base64 decode: feed the encoded form back;
         * reported size stays n (decoded payload bytes) to
         * match the Rust/Go harness convention. */
        EVP_EncodeBlock(enc, buf, (int)n);
        size_t enc_len = strlen((char *)enc);
        uint8_t *dec = malloc(((enc_len / 4) + 1) * 3);
        run_one("b64-decode", op_b64_decode, enc, enc_len, dec, 0, n);

        free(buf);
        free(enc);
        free(dec);
    }
    return 0;
}
