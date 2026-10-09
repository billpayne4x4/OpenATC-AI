// SPDX-FileCopyrightText: 2024 Julia DeMille <me@jdemille.com>
//
// SPDX-License-Identifier: MIT

#include <stdio.h>
#include <stdlib.h>
#include "XPLMMap.h"

XPLM_API XPLMMapLayerID XPLMCreateMapLayer(XPLMCreateMapLayer_t * inParams) {
    fprintf(stderr, "\n\ncalled into a stub library. cannot continue, aborting");
    fprintf(stderr, "this really should not happen. check your dynamic libraries.\n\n");
    abort();
}

XPLM_API int XPLMDestroyMapLayer(XPLMMapLayerID inLayer) {
    fprintf(stderr, "\n\ncalled into a stub library. cannot continue, aborting");
    fprintf(stderr, "this really should not happen. check your dynamic libraries.\n\n");
    abort();
}

XPLM_API void XPLMRegisterMapCreationHook(XPLMMapCreatedCallback_f callback, void * refcon) {
    fprintf(stderr, "\n\ncalled into a stub library. cannot continue, aborting");
    fprintf(stderr, "this really should not happen. check your dynamic libraries.\n\n");
    abort();
}

XPLM_API int XPLMMapExists(const char * mapIdentifier) {
    fprintf(stderr, "\n\ncalled into a stub library. cannot continue, aborting");
    fprintf(stderr, "this really should not happen. check your dynamic libraries.\n\n");
    abort();
}

XPLM_API void XPLMDrawMapIconFromSheet(XPLMMapLayerID layer, const char * inPngPath, int s, int t, int ds, int dt, float mapX, float mapY, XPLMMapOrientation orientation, float rotationDegrees, float mapWidth) {
    fprintf(stderr, "\n\ncalled into a stub library. cannot continue, aborting");
    fprintf(stderr, "this really should not happen. check your dynamic libraries.\n\n");
    abort();
}

XPLM_API void XPLMDrawMapLabel(XPLMMapLayerID layer, const char * inText, float mapX, float mapY, XPLMMapOrientation orientation, float rotationDegrees) {
    fprintf(stderr, "\n\ncalled into a stub library. cannot continue, aborting");
    fprintf(stderr, "this really should not happen. check your dynamic libraries.\n\n");
    abort();
}

XPLM_API void XPLMMapProject(XPLMMapProjectionID projection, double latitude, double longitude, float * outX, float * outY) {
    fprintf(stderr, "\n\ncalled into a stub library. cannot continue, aborting");
    fprintf(stderr, "this really should not happen. check your dynamic libraries.\n\n");
    abort();
}

XPLM_API void XPLMMapUnproject(XPLMMapProjectionID projection, float mapX, float mapY, double * outLatitude, double * outLongitude) {
    fprintf(stderr, "\n\ncalled into a stub library. cannot continue, aborting");
    fprintf(stderr, "this really should not happen. check your dynamic libraries.\n\n");
    abort();
}

XPLM_API float XPLMMapScaleMeter(XPLMMapProjectionID projection, float mapX, float mapY) {
    fprintf(stderr, "\n\ncalled into a stub library. cannot continue, aborting");
    fprintf(stderr, "this really should not happen. check your dynamic libraries.\n\n");
    abort();
}

XPLM_API float XPLMMapGetNorthHeading(XPLMMapProjectionID projection, float mapX, float mapY) {
    fprintf(stderr, "\n\ncalled into a stub library. cannot continue, aborting");
    fprintf(stderr, "this really should not happen. check your dynamic libraries.\n\n");
    abort();
}

