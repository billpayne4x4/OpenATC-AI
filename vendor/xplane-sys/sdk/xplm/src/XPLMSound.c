// SPDX-FileCopyrightText: 2024 Julia DeMille <me@jdemille.com>
//
// SPDX-License-Identifier: MIT

#include <stdio.h>
#include <stdlib.h>
#include "XPLMSound.h"

XPLM_API FMOD_STUDIO_SYSTEM * XPLMGetFMODStudio(void) {
    fprintf(stderr, "\n\ncalled into a stub library. cannot continue, aborting");
    fprintf(stderr, "this really should not happen. check your dynamic libraries.\n\n");
    abort();
}

XPLM_API FMOD_CHANNELGROUP * XPLMGetFMODChannelGroup(XPLMAudioBus audioType) {
    fprintf(stderr, "\n\ncalled into a stub library. cannot continue, aborting");
    fprintf(stderr, "this really should not happen. check your dynamic libraries.\n\n");
    abort();
}

XPLM_API FMOD_CHANNEL * XPLMPlayPCMOnBus(void * audioBuffer, uint32_t bufferSize, FMOD_SOUND_FORMAT soundFormat, int freqHz, int numChannels, int loop, XPLMAudioBus audioType, XPLMPCMComplete_f inCallback, void * inRefcon) {
    fprintf(stderr, "\n\ncalled into a stub library. cannot continue, aborting");
    fprintf(stderr, "this really should not happen. check your dynamic libraries.\n\n");
    abort();
}

XPLM_API FMOD_RESULT XPLMStopAudio(FMOD_CHANNEL * fmod_channel) {
    fprintf(stderr, "\n\ncalled into a stub library. cannot continue, aborting");
    fprintf(stderr, "this really should not happen. check your dynamic libraries.\n\n");
    abort();
}

XPLM_API FMOD_RESULT XPLMSetAudioPosition(FMOD_CHANNEL * fmod_channel, FMOD_VECTOR * position, FMOD_VECTOR * velocity) {
    fprintf(stderr, "\n\ncalled into a stub library. cannot continue, aborting");
    fprintf(stderr, "this really should not happen. check your dynamic libraries.\n\n");
    abort();
}

XPLM_API FMOD_RESULT XPLMSetAudioFadeDistance(FMOD_CHANNEL * fmod_channel, float min_fade_distance, float max_fade_distance) {
    fprintf(stderr, "\n\ncalled into a stub library. cannot continue, aborting");
    fprintf(stderr, "this really should not happen. check your dynamic libraries.\n\n");
    abort();
}

XPLM_API FMOD_RESULT XPLMSetAudioVolume(FMOD_CHANNEL * fmod_channel, float source_volume) {
    fprintf(stderr, "\n\ncalled into a stub library. cannot continue, aborting");
    fprintf(stderr, "this really should not happen. check your dynamic libraries.\n\n");
    abort();
}

XPLM_API FMOD_RESULT XPLMSetAudioPitch(FMOD_CHANNEL * fmod_channel, float audio_pitch_hz) {
    fprintf(stderr, "\n\ncalled into a stub library. cannot continue, aborting");
    fprintf(stderr, "this really should not happen. check your dynamic libraries.\n\n");
    abort();
}

XPLM_API FMOD_RESULT XPLMSetAudioCone(FMOD_CHANNEL * fmod_channel, float inside_angle, float outside_angle, float outside_volume, FMOD_VECTOR * orientation) {
    fprintf(stderr, "\n\ncalled into a stub library. cannot continue, aborting");
    fprintf(stderr, "this really should not happen. check your dynamic libraries.\n\n");
    abort();
}

