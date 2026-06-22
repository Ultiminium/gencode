/**
 * g.h — G encoding system C API
 *
 * G is a context-dependent, density-maximized encoding system.
 * Version: 0.1.0
 *
 * Usage:
 *   #include "g.h"
 *   // link with -lg (shared) or libg.a (static)
 *
 * All encode/decode functions:
 *   - Take caller-allocated output buffers
 *   - Use g_encode_max_size() / g_encode_text_max_size() to size buffers
 *   - Return G_OK (0) on success, negative error code on failure
 *   - Store error detail in thread-local storage, retrieve with g_last_error()
 */

#ifndef G_H
#define G_H

#ifdef __cplusplus
extern "C" {
#endif

#include <stdint.h>
#include <stddef.h>

/* ── Error codes ─────────────────────────────────────────────────────────── */

#define G_OK          0   /* success */
#define G_ERR_BUFFER  -1  /* output buffer too small */
#define G_ERR_INVALID -2  /* invalid input */
#define G_ERR_VERSION -3  /* unsupported .gt version */
#define G_ERR_NULL    -4  /* null pointer argument */
#define G_ERR_ENCODE  -5  /* encoding failed internally */

/* ── Version ─────────────────────────────────────────────────────────────── */

/**
 * Get the G library version.
 * Any output pointer may be NULL (will be ignored).
 */
void g_version(int *major, int *minor, int *patch);

/* ── Error retrieval ─────────────────────────────────────────────────────── */

/**
 * Return a pointer to the last error message for this thread.
 * Valid until the next G call on this thread. Never NULL.
 * Do NOT free this pointer.
 */
const char *g_last_error(void);

/* ── Size queries ────────────────────────────────────────────────────────── */

/**
 * Upper bound on binary encoded output size for in_len input bytes, Gn instance n.
 * Use this to allocate the output buffer for g_encode().
 */
size_t g_encode_max_size(size_t in_len, int n);

/**
 * Upper bound on text encoded output size (.gt format).
 * Use this to allocate the output buffer for g_encode_text().
 */
size_t g_encode_text_max_size(size_t in_len, int n);

/**
 * Upper bound on decoded output size for in_len encoded bytes.
 * Use this to allocate the output buffer for g_decode() and g_decode_text().
 */
size_t g_decode_max_size(size_t in_len);

/* ── Instance info ───────────────────────────────────────────────────────── */

/**
 * Get information about a Gn instance.
 * Any output pointer may be NULL.
 *
 * out_valid_count_lo / out_valid_count_hi: lower/upper 64 bits of valid block count
 * out_bits:        bits per block index
 * out_charset_len: number of characters in Gn charset
 *
 * Returns G_OK or G_ERR_INVALID.
 */
int g_info(int n,
           uint64_t *out_valid_count_lo,
           uint64_t *out_valid_count_hi,
           uint32_t *out_bits,
           size_t   *out_charset_len);

/**
 * Fill out with the Gn character set as a null-terminated ASCII string.
 * out_cap must be at least charset_len + 1 (from g_info).
 * out_len set to number of characters written (excluding null terminator).
 *
 * Returns G_OK or G_ERR_BUFFER / G_ERR_INVALID.
 */
int g_charset(int n, uint8_t *out, size_t out_cap, size_t *out_len);

/* ── Binary encode / decode ──────────────────────────────────────────────── */

/**
 * Encode raw bytes into a binary G stream (.g format).
 *
 * in      — input data
 * in_len  — number of input bytes
 * n       — Gn instance (recommended: 28)
 * out     — output buffer (allocate with g_encode_max_size(in_len, n))
 * out_cap — capacity of output buffer in bytes
 * out_len — set to number of bytes written on success
 *
 * Returns G_OK on success, negative error code on failure.
 */
int g_encode(const uint8_t *in, size_t in_len, int n,
             uint8_t *out, size_t out_cap, size_t *out_len);

/**
 * Decode a binary G stream (.g format) back to raw bytes.
 *
 * in      — G-encoded input
 * in_len  — number of input bytes
 * out     — output buffer (allocate with g_decode_max_size(in_len))
 * out_cap — capacity of output buffer in bytes
 * out_len — set to number of bytes written on success
 *
 * Returns G_OK on success, negative error code on failure.
 */
int g_decode(const uint8_t *in, size_t in_len,
             uint8_t *out, size_t out_cap, size_t *out_len);

/* ── Text encode / decode (.gt format) ───────────────────────────────────── */

/**
 * Encode raw bytes into G canonical text format (.gt).
 *
 * Output is a null-terminated UTF-8 string with the header:
 *   Gn/{major}/{minor}/{patch}/{n}/{original_bytes}
 * followed by one block per line, each exactly n characters.
 *
 * out     — output buffer (allocate with g_encode_text_max_size(in_len, n))
 * out_len — set to number of bytes written (excluding null terminator)
 *
 * Returns G_OK on success, negative error code on failure.
 */
int g_encode_text(const uint8_t *in, size_t in_len, int n,
                  uint8_t *out, size_t out_cap, size_t *out_len);

/**
 * Decode G canonical text format (.gt) back to raw bytes.
 *
 * in      — .gt text (UTF-8, does not need to be null-terminated)
 * in_len  — length of input in bytes
 * out     — output buffer (allocate with g_decode_max_size(in_len))
 * out_len — set to number of bytes written on success
 *
 * Returns G_OK on success, G_ERR_VERSION if the file version is unsupported,
 * G_ERR_INVALID for malformed input, negative on other error.
 */
int g_decode_text(const uint8_t *in, size_t in_len,
                  uint8_t *out, size_t out_cap, size_t *out_len);

/* ── Validation ──────────────────────────────────────────────────────────── */

/**
 * Validate a .gt text string.
 * Checks: header format, version, charset membership, adjacency rules.
 *
 * Returns 1 if valid, 0 if invalid.
 * Call g_last_error() for details on failure.
 */
int g_validate(const uint8_t *in, size_t in_len);


/* ── Streaming encode / decode ───────────────────────────────────────────── */

/**
 * Opaque streaming encoder handle.
 * Allows encoding large data in chunks without loading everything into memory.
 * State (LZ77 window, RNCE model, partial block buffer) persists across chunks.
 *
 * Usage:
 *   g_encoder_t *enc = g_encoder_new(28);
 *   g_encoder_feed(enc, chunk1, len1, out, cap, &out_len); write(out, out_len);
 *   g_encoder_feed(enc, chunk2, len2, out, cap, &out_len); write(out, out_len);
 *   g_encoder_finish(enc, out, cap, &out_len);             write(out, out_len);
 *   g_encoder_free(enc);
 */
typedef struct GEncoderHandle g_encoder_t;

/** Create a new streaming encoder for Gn instance `n`. Returns NULL on error. */
g_encoder_t *g_encoder_new(int n);

/**
 * Feed a chunk of input to the encoder.
 * Appends encoded bytes to out[0..out_cap]. Sets *out_len to bytes written.
 * Returns G_OK on success. out_cap should be at least g_encode_max_size(len, n).
 */
int g_encoder_feed(g_encoder_t *enc,
                   const uint8_t *data, size_t len,
                   uint8_t *out, size_t out_cap, size_t *out_len);

/**
 * Finish encoding. Flushes remaining data and writes final metadata.
 * Must be called exactly once after all g_encoder_feed() calls.
 */
int g_encoder_finish(g_encoder_t *enc,
                     uint8_t *out, size_t out_cap, size_t *out_len);

/** Free the encoder. Always call this, even if encoding failed. */
void g_encoder_free(g_encoder_t *enc);

/* ── Convenience macros ──────────────────────────────────────────────────── */

/** Check return code and jump to label on error */
#define G_CHECK(rc, label) do { if ((rc) != G_OK) goto label; } while(0)

/** Default recommended Gn instance */
#define G_DEFAULT_N 28

#ifdef __cplusplus
} /* extern "C" */
#endif

#endif /* G_H */
