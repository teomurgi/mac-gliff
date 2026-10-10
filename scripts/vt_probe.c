/* Minimal VideoToolbox probe: FFmpeg's exact decode flow over the first
 * access unit of the gliff fixture, to tell a broken runner VT from a
 * bug in gliff-mtl's FFI. Build:
 *   clang -O1 -Wall -o vt_probe vt_probe.c -framework CoreFoundation \
 *       -framework CoreMedia -framework CoreVideo -framework VideoToolbox
 */
#include <CoreFoundation/CoreFoundation.h>
#include <CoreMedia/CoreMedia.h>
#include <CoreVideo/CoreVideo.h>
#include <VideoToolbox/VideoToolbox.h>
#include <dispatch/dispatch.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

static size_t nal_starts(const uint8_t *data, size_t len, size_t *out) {
    size_t n = 0;
    for (size_t i = 0; i + 3 <= len; i++) {
        if (data[i] == 0 && data[i + 1] == 0 && data[i + 2] == 1) {
            out[n++] = i + 3;
            i += 2;
        }
    }
    return n;
}

static void on_output(void *ctx, void *frame, OSStatus status,
                      VTDecodeInfoFlags flags, CVImageBufferRef image,
                      CMTime pts, CMTime dur) {
    (void)ctx;
    (void)frame;
    (void)flags;
    (void)pts;
    (void)dur;
    if (status == 0 && image) {
        size_t w = CVPixelBufferGetWidth(image);
        size_t h = CVPixelBufferGetHeight(image);
        printf("probe: decoded %zux%zu\n", w, h);
    } else {
        printf("probe: callback status %d\n", (int)status);
    }
    dispatch_semaphore_signal((dispatch_semaphore_t)ctx);
}

int main(int argc, char **argv) {
    if (argc != 2) {
        fprintf(stderr, "usage: vt_probe <stream.264>\n");
        return 2;
    }
    FILE *f = fopen(argv[1], "rb");
    if (!f) {
        perror("open");
        return 2;
    }
    fseek(f, 0, SEEK_END);
    long size = ftell(f);
    fseek(f, 0, SEEK_SET);
    uint8_t *data = malloc(size);
    if (fread(data, 1, size, f) != (size_t)size) {
        return 2;
    }
    fclose(f);

    /* First access unit: NALs up to the second slice. */
    size_t starts[256];
    size_t n = nal_starts(data, size, starts);
    const uint8_t *sps = NULL, *pps = NULL;
    size_t sps_len = 0, pps_len = 0;
    uint8_t *au = malloc(size);
    size_t au_len = 0;
    int seen_slice = 0;
    for (size_t i = 0; i < n; i++) {
        size_t body = starts[i];
        size_t end = (i + 1 < n) ? starts[i + 1] - 3 : size;
        while (end > body && data[end - 1] == 0) end--;
        uint8_t type = data[body] & 0x1f;
        if ((type == 1 || type == 5) && seen_slice) break;
        if (type == 7) {
            sps = data + body;
            sps_len = end - body;
        } else if (type == 8) {
            pps = data + body;
            pps_len = end - body;
        }
        if (type == 1 || type == 5) seen_slice = 1;
        if (type != 7 && type != 8 && type != 9) {
            uint32_t len = (uint32_t)(end - body);
            len = ((len >> 24) & 0xff) | ((len >> 8) & 0xff00) |
                  ((len << 8) & 0xff0000) | (len << 24);
            memcpy(au + au_len, &len, 4);
            au_len += 4;
            memcpy(au + au_len, data + body, end - body);
            au_len += end - body;
        }
    }
    printf("probe: au %zu bytes, sps %zu, pps %zu\n", au_len, sps_len, pps_len);
    if (!sps || !pps) {
        fprintf(stderr, "probe: no parameter sets\n");
        return 1;
    }

    CMVideoFormatDescriptionRef fmt = NULL;
    const uint8_t *ps[2] = {sps, pps};
    size_t ps_len[2] = {sps_len, pps_len};
    OSStatus st = CMVideoFormatDescriptionCreateFromH264ParameterSets(
        NULL, 2, ps, ps_len, 4, &fmt);
    printf("probe: fmt %d\n", (int)st);
    if (st) return 1;

    int32_t pixel_format = kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange;
    CFNumberRef format_number =
        CFNumberCreate(NULL, kCFNumberSInt32Type, &pixel_format);
    const void *keys[] = {kCVPixelBufferPixelFormatTypeKey,
                          kCVPixelBufferMetalCompatibilityKey};
    const void *values[] = {format_number, kCFBooleanTrue};
    CFDictionaryRef attrs = CFDictionaryCreate(
        NULL, keys, values, 2, &kCFTypeDictionaryKeyCallBacks,
        &kCFTypeDictionaryValueCallBacks);
    CFRelease(format_number);

    const void *skeys[] = {
        kVTVideoDecoderSpecification_EnableHardwareAcceleratedVideoDecoder};
    const void *svalues[] = {kCFBooleanFalse};
    CFDictionaryRef spec = CFDictionaryCreate(
        NULL, skeys, svalues, 1, &kCFTypeDictionaryKeyCallBacks,
        &kCFTypeDictionaryValueCallBacks);

    dispatch_semaphore_t sem = dispatch_semaphore_create(0);
    VTDecompressionOutputCallbackRecord cb = {on_output, sem};
    VTDecompressionSessionRef session = NULL;
    st = VTDecompressionSessionCreate(NULL, fmt, spec, attrs, &cb, &session);
    CFRelease(spec);
    CFRelease(attrs);
    printf("probe: session %d\n", (int)st);
    if (st) return 1;

    CMBlockBufferRef block = NULL;
    st = CMBlockBufferCreateWithMemoryBlock(
        kCFAllocatorDefault, au, au_len, kCFAllocatorNull, NULL, 0, au_len, 0,
        &block);
    printf("probe: block %d\n", (int)st);
    if (st) return 1;

    CMSampleTimingInfo timing = {kCMTimeInvalid,
                                 CMTimeMake(1, 600), kCMTimeInvalid};
    CMSampleBufferRef sb = NULL;
    st = CMSampleBufferCreateReady(NULL, block, fmt, 1, 1, &timing, 0, NULL,
                                   &sb);
    printf("probe: sample %d\n", (int)st);
    if (st) return 1;
    st = VTDecompressionSessionDecodeFrame(session, sb, 0, NULL, NULL);
    printf("probe: decode %d\n", (int)st);
    dispatch_semaphore_wait(sem, DISPATCH_TIME_FOREVER);
    printf("probe: done\n");
    return 0;
}
