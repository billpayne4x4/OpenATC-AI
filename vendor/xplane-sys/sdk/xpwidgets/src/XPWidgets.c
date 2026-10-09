// SPDX-FileCopyrightText: 2024 Julia DeMille <me@jdemille.com>
//
// SPDX-License-Identifier: MIT

#include <stdio.h>
#include <stdlib.h>
#include "XPWidgets.h"

WIDGET_API XPWidgetID XPCreateWidget(int inLeft, int inTop, int inRight, int inBottom, int inVisible, const char * inDescriptor, int inIsRoot, XPWidgetID inContainer, XPWidgetClass inClass) {
    fprintf(stderr, "\n\ncalled into a stub library. cannot continue, aborting");
    fprintf(stderr, "this really should not happen. check your dynamic libraries.\n\n");
    abort();
}

WIDGET_API XPWidgetID XPCreateCustomWidget(int inLeft, int inTop, int inRight, int inBottom, int inVisible, const char * inDescriptor, int inIsRoot, XPWidgetID inContainer, XPWidgetFunc_t inCallback) {
    fprintf(stderr, "\n\ncalled into a stub library. cannot continue, aborting");
    fprintf(stderr, "this really should not happen. check your dynamic libraries.\n\n");
    abort();
}

WIDGET_API void XPDestroyWidget(XPWidgetID inWidget, int inDestroyChildren) {
    fprintf(stderr, "\n\ncalled into a stub library. cannot continue, aborting");
    fprintf(stderr, "this really should not happen. check your dynamic libraries.\n\n");
    abort();
}

WIDGET_API int XPSendMessageToWidget(XPWidgetID inWidget, XPWidgetMessage inMessage, XPDispatchMode inMode, intptr_t inParam1, intptr_t inParam2) {
    fprintf(stderr, "\n\ncalled into a stub library. cannot continue, aborting");
    fprintf(stderr, "this really should not happen. check your dynamic libraries.\n\n");
    abort();
}

WIDGET_API void XPPlaceWidgetWithin(XPWidgetID inSubWidget, XPWidgetID inContainer) {
    fprintf(stderr, "\n\ncalled into a stub library. cannot continue, aborting");
    fprintf(stderr, "this really should not happen. check your dynamic libraries.\n\n");
    abort();
}

WIDGET_API int XPCountChildWidgets(XPWidgetID inWidget) {
    fprintf(stderr, "\n\ncalled into a stub library. cannot continue, aborting");
    fprintf(stderr, "this really should not happen. check your dynamic libraries.\n\n");
    abort();
}

WIDGET_API XPWidgetID XPGetNthChildWidget(XPWidgetID inWidget, int inIndex) {
    fprintf(stderr, "\n\ncalled into a stub library. cannot continue, aborting");
    fprintf(stderr, "this really should not happen. check your dynamic libraries.\n\n");
    abort();
}

WIDGET_API XPWidgetID XPGetParentWidget(XPWidgetID inWidget) {
    fprintf(stderr, "\n\ncalled into a stub library. cannot continue, aborting");
    fprintf(stderr, "this really should not happen. check your dynamic libraries.\n\n");
    abort();
}

WIDGET_API void XPShowWidget(XPWidgetID inWidget) {
    fprintf(stderr, "\n\ncalled into a stub library. cannot continue, aborting");
    fprintf(stderr, "this really should not happen. check your dynamic libraries.\n\n");
    abort();
}

WIDGET_API void XPHideWidget(XPWidgetID inWidget) {
    fprintf(stderr, "\n\ncalled into a stub library. cannot continue, aborting");
    fprintf(stderr, "this really should not happen. check your dynamic libraries.\n\n");
    abort();
}

WIDGET_API int XPIsWidgetVisible(XPWidgetID inWidget) {
    fprintf(stderr, "\n\ncalled into a stub library. cannot continue, aborting");
    fprintf(stderr, "this really should not happen. check your dynamic libraries.\n\n");
    abort();
}

WIDGET_API XPWidgetID XPFindRootWidget(XPWidgetID inWidget) {
    fprintf(stderr, "\n\ncalled into a stub library. cannot continue, aborting");
    fprintf(stderr, "this really should not happen. check your dynamic libraries.\n\n");
    abort();
}

WIDGET_API void XPBringRootWidgetToFront(XPWidgetID inWidget) {
    fprintf(stderr, "\n\ncalled into a stub library. cannot continue, aborting");
    fprintf(stderr, "this really should not happen. check your dynamic libraries.\n\n");
    abort();
}

WIDGET_API int XPIsWidgetInFront(XPWidgetID inWidget) {
    fprintf(stderr, "\n\ncalled into a stub library. cannot continue, aborting");
    fprintf(stderr, "this really should not happen. check your dynamic libraries.\n\n");
    abort();
}

WIDGET_API void XPGetWidgetGeometry(XPWidgetID inWidget, int * outLeft, int * outTop, int * outRight, int * outBottom) {
    fprintf(stderr, "\n\ncalled into a stub library. cannot continue, aborting");
    fprintf(stderr, "this really should not happen. check your dynamic libraries.\n\n");
    abort();
}

WIDGET_API void XPSetWidgetGeometry(XPWidgetID inWidget, int inLeft, int inTop, int inRight, int inBottom) {
    fprintf(stderr, "\n\ncalled into a stub library. cannot continue, aborting");
    fprintf(stderr, "this really should not happen. check your dynamic libraries.\n\n");
    abort();
}

WIDGET_API XPWidgetID XPGetWidgetForLocation(XPWidgetID inContainer, int inXOffset, int inYOffset, int inRecursive, int inVisibleOnly) {
    fprintf(stderr, "\n\ncalled into a stub library. cannot continue, aborting");
    fprintf(stderr, "this really should not happen. check your dynamic libraries.\n\n");
    abort();
}

WIDGET_API void XPGetWidgetExposedGeometry(XPWidgetID inWidgetID, int * outLeft, int * outTop, int * outRight, int * outBottom) {
    fprintf(stderr, "\n\ncalled into a stub library. cannot continue, aborting");
    fprintf(stderr, "this really should not happen. check your dynamic libraries.\n\n");
    abort();
}

WIDGET_API void XPSetWidgetDescriptor(XPWidgetID inWidget, const char * inDescriptor) {
    fprintf(stderr, "\n\ncalled into a stub library. cannot continue, aborting");
    fprintf(stderr, "this really should not happen. check your dynamic libraries.\n\n");
    abort();
}

WIDGET_API int XPGetWidgetDescriptor(XPWidgetID inWidget, char * outDescriptor, int inMaxDescLength) {
    fprintf(stderr, "\n\ncalled into a stub library. cannot continue, aborting");
    fprintf(stderr, "this really should not happen. check your dynamic libraries.\n\n");
    abort();
}

WIDGET_API XPLMWindowID XPGetWidgetUnderlyingWindow(XPWidgetID inWidget) {
    fprintf(stderr, "\n\ncalled into a stub library. cannot continue, aborting");
    fprintf(stderr, "this really should not happen. check your dynamic libraries.\n\n");
    abort();
}

WIDGET_API void XPSetWidgetProperty(XPWidgetID inWidget, XPWidgetPropertyID inProperty, intptr_t inValue) {
    fprintf(stderr, "\n\ncalled into a stub library. cannot continue, aborting");
    fprintf(stderr, "this really should not happen. check your dynamic libraries.\n\n");
    abort();
}

WIDGET_API intptr_t XPGetWidgetProperty(XPWidgetID inWidget, XPWidgetPropertyID inProperty, int * inExists) {
    fprintf(stderr, "\n\ncalled into a stub library. cannot continue, aborting");
    fprintf(stderr, "this really should not happen. check your dynamic libraries.\n\n");
    abort();
}

WIDGET_API XPWidgetID XPSetKeyboardFocus(XPWidgetID inWidget) {
    fprintf(stderr, "\n\ncalled into a stub library. cannot continue, aborting");
    fprintf(stderr, "this really should not happen. check your dynamic libraries.\n\n");
    abort();
}

WIDGET_API void XPLoseKeyboardFocus(XPWidgetID inWidget) {
    fprintf(stderr, "\n\ncalled into a stub library. cannot continue, aborting");
    fprintf(stderr, "this really should not happen. check your dynamic libraries.\n\n");
    abort();
}

WIDGET_API XPWidgetID XPGetWidgetWithFocus(void) {
    fprintf(stderr, "\n\ncalled into a stub library. cannot continue, aborting");
    fprintf(stderr, "this really should not happen. check your dynamic libraries.\n\n");
    abort();
}

WIDGET_API void XPAddWidgetCallback(XPWidgetID inWidget, XPWidgetFunc_t inNewCallback) {
    fprintf(stderr, "\n\ncalled into a stub library. cannot continue, aborting");
    fprintf(stderr, "this really should not happen. check your dynamic libraries.\n\n");
    abort();
}

WIDGET_API XPWidgetFunc_t XPGetWidgetClassFunc(XPWidgetClass inWidgetClass) {
    fprintf(stderr, "\n\ncalled into a stub library. cannot continue, aborting");
    fprintf(stderr, "this really should not happen. check your dynamic libraries.\n\n");
    abort();
}

void meaningless(void) {
    XPLMUnregisterAvionicsCallbacks(NULL);
}
