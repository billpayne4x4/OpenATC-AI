// SPDX-FileCopyrightText: 2024 Julia DeMille <me@jdemille.com>
//
// SPDX-License-Identifier: MIT

#include <stdio.h>
#include <stdlib.h>
#include "XPLMPlanes.h"

XPLM_API void XPLMSetUsersAircraft(const char * inAircraftPath) {
    fprintf(stderr, "\n\ncalled into a stub library. cannot continue, aborting");
    fprintf(stderr, "this really should not happen. check your dynamic libraries.\n\n");
    abort();
}

XPLM_API void XPLMPlaceUserAtAirport(const char * inAirportCode) {
    fprintf(stderr, "\n\ncalled into a stub library. cannot continue, aborting");
    fprintf(stderr, "this really should not happen. check your dynamic libraries.\n\n");
    abort();
}

XPLM_API void XPLMPlaceUserAtLocation(double latitudeDegrees, double longitudeDegrees, float elevationMetersMSL, float headingDegreesTrue, float speedMetersPerSecond) {
    fprintf(stderr, "\n\ncalled into a stub library. cannot continue, aborting");
    fprintf(stderr, "this really should not happen. check your dynamic libraries.\n\n");
    abort();
}

XPLM_API void XPLMCountAircraft(int * outTotalAircraft, int * outActiveAircraft, XPLMPluginID * outController) {
    fprintf(stderr, "\n\ncalled into a stub library. cannot continue, aborting");
    fprintf(stderr, "this really should not happen. check your dynamic libraries.\n\n");
    abort();
}

XPLM_API void XPLMGetNthAircraftModel(int inIndex, char * outFileName, char * outPath) {
    fprintf(stderr, "\n\ncalled into a stub library. cannot continue, aborting");
    fprintf(stderr, "this really should not happen. check your dynamic libraries.\n\n");
    abort();
}

XPLM_API int XPLMAcquirePlanes(char ** inAircraft, XPLMPlanesAvailable_f inCallback, void * inRefcon) {
    fprintf(stderr, "\n\ncalled into a stub library. cannot continue, aborting");
    fprintf(stderr, "this really should not happen. check your dynamic libraries.\n\n");
    abort();
}

XPLM_API void XPLMReleasePlanes(void) {
    fprintf(stderr, "\n\ncalled into a stub library. cannot continue, aborting");
    fprintf(stderr, "this really should not happen. check your dynamic libraries.\n\n");
    abort();
}

XPLM_API void XPLMSetActiveAircraftCount(int inCount) {
    fprintf(stderr, "\n\ncalled into a stub library. cannot continue, aborting");
    fprintf(stderr, "this really should not happen. check your dynamic libraries.\n\n");
    abort();
}

XPLM_API void XPLMSetAircraftModel(int inIndex, const char * inAircraftPath) {
    fprintf(stderr, "\n\ncalled into a stub library. cannot continue, aborting");
    fprintf(stderr, "this really should not happen. check your dynamic libraries.\n\n");
    abort();
}

XPLM_API void XPLMDisableAIForPlane(int inPlaneIndex) {
    fprintf(stderr, "\n\ncalled into a stub library. cannot continue, aborting");
    fprintf(stderr, "this really should not happen. check your dynamic libraries.\n\n");
    abort();
}

XPLM_API void XPLMDrawAircraft(int inPlaneIndex, float inX, float inY, float inZ, float inPitch, float inRoll, float inYaw, int inFullDraw, XPLMPlaneDrawState_t * inDrawStateInfo) {
    fprintf(stderr, "\n\ncalled into a stub library. cannot continue, aborting");
    fprintf(stderr, "this really should not happen. check your dynamic libraries.\n\n");
    abort();
}

XPLM_API void XPLMReinitUsersPlane(void) {
    fprintf(stderr, "\n\ncalled into a stub library. cannot continue, aborting");
    fprintf(stderr, "this really should not happen. check your dynamic libraries.\n\n");
    abort();
}

