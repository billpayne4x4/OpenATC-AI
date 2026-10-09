// SPDX-FileCopyrightText: 2024 Julia DeMille <me@jdemille.com>
//
// SPDX-License-Identifier: MIT

#include <stdio.h>
#include <stdlib.h>
#include "XPLMGraphics.h"

XPLM_API void XPLMSetGraphicsState(int inEnableFog, int inNumberTexUnits, int inEnableLighting, int inEnableAlphaTesting, int inEnableAlphaBlending, int inEnableDepthTesting, int inEnableDepthWriting) {
    fprintf(stderr, "\n\ncalled into a stub library. cannot continue, aborting");
    fprintf(stderr, "this really should not happen. check your dynamic libraries.\n\n");
    abort();
}

XPLM_API void XPLMBindTexture2d(int inTextureNum, int inTextureUnit) {
    fprintf(stderr, "\n\ncalled into a stub library. cannot continue, aborting");
    fprintf(stderr, "this really should not happen. check your dynamic libraries.\n\n");
    abort();
}

XPLM_API void XPLMGenerateTextureNumbers(int * outTextureIDs, int inCount) {
    fprintf(stderr, "\n\ncalled into a stub library. cannot continue, aborting");
    fprintf(stderr, "this really should not happen. check your dynamic libraries.\n\n");
    abort();
}

XPLM_API int XPLMGetTexture(XPLMTextureID inTexture) {
    fprintf(stderr, "\n\ncalled into a stub library. cannot continue, aborting");
    fprintf(stderr, "this really should not happen. check your dynamic libraries.\n\n");
    abort();
}

XPLM_API void XPLMWorldToLocal(double inLatitude, double inLongitude, double inAltitude, double * outX, double * outY, double * outZ) {
    fprintf(stderr, "\n\ncalled into a stub library. cannot continue, aborting");
    fprintf(stderr, "this really should not happen. check your dynamic libraries.\n\n");
    abort();
}

XPLM_API void XPLMLocalToWorld(double inX, double inY, double inZ, double * outLatitude, double * outLongitude, double * outAltitude) {
    fprintf(stderr, "\n\ncalled into a stub library. cannot continue, aborting");
    fprintf(stderr, "this really should not happen. check your dynamic libraries.\n\n");
    abort();
}

XPLM_API void XPLMDrawTranslucentDarkBox(int inLeft, int inTop, int inRight, int inBottom) {
    fprintf(stderr, "\n\ncalled into a stub library. cannot continue, aborting");
    fprintf(stderr, "this really should not happen. check your dynamic libraries.\n\n");
    abort();
}

XPLM_API void XPLMDrawString(float * inColorRGB, int inXOffset, int inYOffset, char * inChar, int * inWordWrapWidth, XPLMFontID inFontID) {
    fprintf(stderr, "\n\ncalled into a stub library. cannot continue, aborting");
    fprintf(stderr, "this really should not happen. check your dynamic libraries.\n\n");
    abort();
}

XPLM_API void XPLMDrawNumber(float * inColorRGB, int inXOffset, int inYOffset, double inValue, int inDigits, int inDecimals, int inShowSign, XPLMFontID inFontID) {
    fprintf(stderr, "\n\ncalled into a stub library. cannot continue, aborting");
    fprintf(stderr, "this really should not happen. check your dynamic libraries.\n\n");
    abort();
}

XPLM_API void XPLMGetFontDimensions(XPLMFontID inFontID, int * outCharWidth, int * outCharHeight, int * outDigitsOnly) {
    fprintf(stderr, "\n\ncalled into a stub library. cannot continue, aborting");
    fprintf(stderr, "this really should not happen. check your dynamic libraries.\n\n");
    abort();
}

XPLM_API float XPLMMeasureString(XPLMFontID inFontID, const char * inChar, int inNumChars) {
    fprintf(stderr, "\n\ncalled into a stub library. cannot continue, aborting");
    fprintf(stderr, "this really should not happen. check your dynamic libraries.\n\n");
    abort();
}

