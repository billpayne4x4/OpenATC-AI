/* Narrow C shim over miniaudio 0.11.22 for the Rust audio engine.
 *
 * Rust never sees miniaudio structs: every handle is heap-allocated here and
 * handed over as an opaque pointer. Device lookup uses the cached enumeration index
 * ("name #index" keys are assembled on the Rust side). Both handle structs
 * start with their ma_device, so start/stop/close take either handle type.
 */
#define OATC_NO_PULSE_MONITORS
#define MINIAUDIO_IMPLEMENTATION
#include "miniaudio.h"

#include <stdlib.h>
#include <string.h>

typedef void (*oatc_capture_cb)(void *ud, const short *samples, unsigned count);
typedef void (*oatc_render_cb)(void *ud, float *out, unsigned frames);

typedef struct {
    ma_device device;
    oatc_capture_cb callback;
    void *ud;
} oatc_capture_t;

typedef struct {
    ma_device device;
    oatc_render_cb callback;
    void *ud;
} oatc_render_t;

/* Cache IDs from the names snapshot. Hotplug must not silently reorder a selection. */
#define OATC_MAX_DEVICES 32
typedef struct {
    ma_context audio;
    ma_bool32 initialized;
    ma_device_id inputs[OATC_MAX_DEVICES];
    ma_device_id outputs[OATC_MAX_DEVICES];
    int input_count;
    int output_count;
} oatc_context_t;

oatc_context_t *oatc_context_new(void) {
    oatc_context_t *context = (oatc_context_t *)calloc(1, sizeof(oatc_context_t));
    return context;
}

void oatc_context_free(oatc_context_t *context) {
    if (context != NULL) {
        if (context->initialized) ma_context_uninit(&context->audio);
        free(context);
    }
}

int oatc_context_init(oatc_context_t *context) {
    if (context == NULL) {
        return -1;
    }
    context->initialized = ma_context_init(NULL, 0, NULL, &context->audio) == MA_SUCCESS;
    return context->initialized ? 0 : -1;
}

/// Copy up to max capture names in ONE enumeration. Returns the count, or
/// -1 on failure. Single-call so no cross-call lifetimes can dangle.
int oatc_capture_list(oatc_context_t *context, char names[][256], int max) {
    ma_device_info *inputs = NULL;
    ma_uint32 count = 0;
    int copied = 0;
    int index;
    context->input_count = 0;
    if (max > OATC_MAX_DEVICES) max = OATC_MAX_DEVICES;
    if (max <= 0) {
        return 0;
    }
    if (ma_context_get_devices(&context->audio, NULL, NULL, &inputs, &count) != MA_SUCCESS) {
        return -1;
    }
    for (index = 0; index < (int)count && index < max; ++index) {
        strncpy(names[index], inputs[index].name, 255);
        names[index][255] = '\0';
        context->inputs[index] = inputs[index].id;
        ++copied;
    }
    context->input_count = copied;
    return copied;
}

/// Copy up to max playback names in ONE enumeration. See capture version.
int oatc_playback_list(oatc_context_t *context, char names[][256], int max) {
    ma_device_info *outputs = NULL;
    ma_uint32 count = 0;
    int copied = 0;
    int index;
    context->output_count = 0;
    if (max > OATC_MAX_DEVICES) max = OATC_MAX_DEVICES;
    if (max <= 0) {
        return 0;
    }
    if (ma_context_get_devices(&context->audio, &outputs, &count, NULL, NULL) != MA_SUCCESS) {
        return -1;
    }
    for (index = 0; index < (int)count && index < max; ++index) {
        strncpy(names[index], outputs[index].name, 255);
        names[index][255] = '\0';
        context->outputs[index] = outputs[index].id;
        ++copied;
    }
    context->output_count = copied;
    return copied;
}

static void capture_data(ma_device *device, void *output, const void *input, ma_uint32 count) {
    oatc_capture_t *handle = (oatc_capture_t *)device->pUserData;
    (void)output;
    if (input == NULL || count == 0 || handle == NULL || handle->callback == NULL) {
        return;
    }
    handle->callback(handle->ud, (const short *)input, (unsigned)count);
}

oatc_capture_t *oatc_capture_open(oatc_context_t *context, int index, oatc_capture_cb callback,
                                  void *ud) {
    const ma_device_id *id = NULL;
    ma_device_id selected;
    if (index >= 0) {
        if (index >= context->input_count) return NULL;
        selected = context->inputs[index];
        id = &selected;
    }
    oatc_capture_t *handle = (oatc_capture_t *)calloc(1, sizeof(oatc_capture_t));
    if (handle == NULL) {
        return NULL;
    }
    ma_device_config config = ma_device_config_init(ma_device_type_capture);
    config.capture.pDeviceID = id;
    config.capture.format = ma_format_s16;
    config.capture.channels = 1;
    config.sampleRate = 16000;
    config.pUserData = handle;
    config.dataCallback = capture_data;
    handle->callback = callback;
    handle->ud = ud;
    if (ma_device_init(&context->audio, &config, &handle->device) != MA_SUCCESS) {
        free(handle);
        return NULL;
    }
    return handle;
}

static void render_data(ma_device *device, void *output, const void *input, ma_uint32 frames) {
    oatc_render_t *handle = (oatc_render_t *)device->pUserData;
    (void)input;
    if (handle == NULL || handle->callback == NULL) {
        return;
    }
    handle->callback(handle->ud, (float *)output, (unsigned)frames);
}

oatc_render_t *oatc_render_open(oatc_context_t *context, int index, oatc_render_cb callback,
                                void *ud) {
    const ma_device_id *id = NULL;
    ma_device_id selected;
    if (index >= 0) {
        if (index >= context->output_count) return NULL;
        selected = context->outputs[index];
        id = &selected;
    }
    oatc_render_t *handle = (oatc_render_t *)calloc(1, sizeof(oatc_render_t));
    if (handle == NULL) {
        return NULL;
    }
    ma_device_config config = ma_device_config_init(ma_device_type_playback);
    config.playback.pDeviceID = id;
    config.playback.format = ma_format_f32;
    config.playback.channels = 2;
    config.sampleRate = 48000;
    config.pUserData = handle;
    config.dataCallback = render_data;
    handle->callback = callback;
    handle->ud = ud;
    if (ma_device_init(&context->audio, &config, &handle->device) != MA_SUCCESS) {
        free(handle);
        return NULL;
    }
    return handle;
}

int oatc_stream_start(void *handle) {
    if (handle == NULL) {
        return -1;
    }
    return ma_device_start((ma_device *)handle) == MA_SUCCESS ? 0 : -1;
}

int oatc_stream_stop(void *handle) {
    if (handle == NULL) {
        return -1;
    }
    return ma_device_stop((ma_device *)handle) == MA_SUCCESS ? 0 : -1;
}

void oatc_stream_close(void *handle) {
    if (handle == NULL) {
        return;
    }
    ma_device_uninit((ma_device *)handle);
    free(handle);
}

ma_decoder *oatc_decode_open(const void *data, unsigned long long length) {
    ma_decoder *decoder = (ma_decoder *)calloc(1, sizeof(ma_decoder));
    if (decoder == NULL) {
        return NULL;
    }
    ma_decoder_config config = ma_decoder_config_init(ma_format_f32, 2, 48000);
    if (ma_decoder_init_memory(data, (size_t)length, &config, decoder) != MA_SUCCESS) {
        free(decoder);
        return NULL;
    }
    return decoder;
}

unsigned long long oatc_decode_read(ma_decoder *decoder, float *out, unsigned long long frames) {
    ma_uint64 read = 0;
    if (decoder == NULL || out == NULL) {
        return 0;
    }
    if (ma_decoder_read_pcm_frames(decoder, out, frames, &read) != MA_SUCCESS) {
        return 0;
    }
    return (unsigned long long)read;
}

unsigned long long oatc_decode_total(ma_decoder *decoder) {
    ma_uint64 total = 0;
    if (decoder == NULL) {
        return 0;
    }
    if (ma_decoder_get_length_in_pcm_frames(decoder, &total) != MA_SUCCESS) {
        return 0;
    }
    return (unsigned long long)total;
}

void oatc_decode_close(ma_decoder *decoder) {
    if (decoder == NULL) {
        return;
    }
    ma_decoder_uninit(decoder);
    free(decoder);
}
