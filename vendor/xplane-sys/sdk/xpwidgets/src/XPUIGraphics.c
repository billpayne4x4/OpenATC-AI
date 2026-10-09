// SPDX-FileCopyrightText: 2024 Julia DeMille <me@jdemille.com>
//
// SPDX-License-Identifier: MIT

#include <stdio.h>
#include <stdlib.h>
#include "XPUIGraphics.h"

WIDGET_API void XPDrawWindow(int inX1, int inY1, int inX2, int inY2, XPWindowStyle inStyle) {
    fprintf(stderr, "\n\ncalled into a stub library. cannot continue, aborting");
    fprintf(stderr, "this really should not happen. check your dynamic libraries.\n\n");
    abort();
}

WIDGET_API void XPGetWindowDefaultDimensions(XPWindowStyle inStyle, int * outWidth, int * outHeight) {
    fprintf(stderr, "\n\ncalled into a stub library. cannot continue, aborting");
    fprintf(stderr, "this really should not happen. check your dynamic libraries.\n\n");
    abort();
}

WIDGET_API void XPDrawElement(int inX1, int inY1, int inX2, int inY2, XPElementStyle inStyle, int inLit) {
    fprintf(stderr, "\n\ncalled into a stub library. cannot continue, aborting");
    fprintf(stderr, "this really should not happen. check your dynamic libraries.\n\n");
    abort();
}

WIDGET_API void XPGetElementDefaultDimensions(XPElementStyle inStyle, int * outWidth, int * outHeight, int * outCanBeLit) {
    fprintf(stderr, "\n\ncalled into a stub library. cannot continue, aborting");
    fprintf(stderr, "this really should not happen. check your dynamic libraries.\n\n");
    abort();
}

WIDGET_API void XPDrawTrack(int inX1, int inY1, int inX2, int inY2, int inMin, int inMax, int inValue, XPTrackStyle inTrackStyle, int inLit) {
    fprintf(stderr, "\n\ncalled into a stub library. cannot continue, aborting");
    fprintf(stderr, "this really should not happen. check your dynamic libraries.\n\n");
    abort();
}

WIDGET_API void XPGetTrackDefaultDimensions(XPTrackStyle inStyle, int * outWidth, int * outCanBeLit) {
    fprintf(stderr, "\n\ncalled into a stub library. cannot continue, aborting");
    fprintf(stderr, "this really should not happen. check your dynamic libraries.\n\n");
    abort();
}

WIDGET_API void XPGetTrackMetrics(int inX1, int inY1, int inX2, int inY2, int inMin, int inMax, int inValue, XPTrackStyle inTrackStyle, int * outIsVertical, int * outDownBtnSize, int * outDownPageSize, int * outThumbSize, int * outUpPageSize, int * outUpBtnSize) {
    fprintf(stderr, "\n\ncalled into a stub library. cannot continue, aborting");
    fprintf(stderr, "this really should not happen. check your dynamic libraries.\n\n");
    abort();
}

