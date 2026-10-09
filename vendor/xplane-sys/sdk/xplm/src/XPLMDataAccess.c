// SPDX-FileCopyrightText: 2024 Julia DeMille <me@jdemille.com>
//
// SPDX-License-Identifier: MIT

#include <stdio.h>
#include <stdlib.h>
#include "XPLMDataAccess.h"

XPLM_API int XPLMCountDataRefs(void) {
    fprintf(stderr, "\n\ncalled into a stub library. cannot continue, aborting");
    fprintf(stderr, "this really should not happen. check your dynamic libraries.\n\n");
    abort();
}

XPLM_API void XPLMGetDataRefsByIndex(int offset, int count, XPLMDataRef * outDataRefs) {
    fprintf(stderr, "\n\ncalled into a stub library. cannot continue, aborting");
    fprintf(stderr, "this really should not happen. check your dynamic libraries.\n\n");
    abort();
}

XPLM_API void XPLMGetDataRefInfo(XPLMDataRef inDataRef, XPLMDataRefInfo_t * outInfo) {
    fprintf(stderr, "\n\ncalled into a stub library. cannot continue, aborting");
    fprintf(stderr, "this really should not happen. check your dynamic libraries.\n\n");
    abort();
}

XPLM_API XPLMDataRef XPLMFindDataRef(const char * inDataRefName) {
    fprintf(stderr, "\n\ncalled into a stub library. cannot continue, aborting");
    fprintf(stderr, "this really should not happen. check your dynamic libraries.\n\n");
    abort();
}

XPLM_API int XPLMCanWriteDataRef(XPLMDataRef inDataRef) {
    fprintf(stderr, "\n\ncalled into a stub library. cannot continue, aborting");
    fprintf(stderr, "this really should not happen. check your dynamic libraries.\n\n");
    abort();
}

XPLM_API int XPLMIsDataRefGood(XPLMDataRef inDataRef) {
    fprintf(stderr, "\n\ncalled into a stub library. cannot continue, aborting");
    fprintf(stderr, "this really should not happen. check your dynamic libraries.\n\n");
    abort();
}

XPLM_API XPLMDataTypeID XPLMGetDataRefTypes(XPLMDataRef inDataRef) {
    fprintf(stderr, "\n\ncalled into a stub library. cannot continue, aborting");
    fprintf(stderr, "this really should not happen. check your dynamic libraries.\n\n");
    abort();
}

XPLM_API int XPLMGetDatai(XPLMDataRef inDataRef) {
    fprintf(stderr, "\n\ncalled into a stub library. cannot continue, aborting");
    fprintf(stderr, "this really should not happen. check your dynamic libraries.\n\n");
    abort();
}

XPLM_API void XPLMSetDatai(XPLMDataRef inDataRef, int inValue) {
    fprintf(stderr, "\n\ncalled into a stub library. cannot continue, aborting");
    fprintf(stderr, "this really should not happen. check your dynamic libraries.\n\n");
    abort();
}

XPLM_API float XPLMGetDataf(XPLMDataRef inDataRef) {
    fprintf(stderr, "\n\ncalled into a stub library. cannot continue, aborting");
    fprintf(stderr, "this really should not happen. check your dynamic libraries.\n\n");
    abort();
}

XPLM_API void XPLMSetDataf(XPLMDataRef inDataRef, float inValue) {
    fprintf(stderr, "\n\ncalled into a stub library. cannot continue, aborting");
    fprintf(stderr, "this really should not happen. check your dynamic libraries.\n\n");
    abort();
}

XPLM_API double XPLMGetDatad(XPLMDataRef inDataRef) {
    fprintf(stderr, "\n\ncalled into a stub library. cannot continue, aborting");
    fprintf(stderr, "this really should not happen. check your dynamic libraries.\n\n");
    abort();
}

XPLM_API void XPLMSetDatad(XPLMDataRef inDataRef, double inValue) {
    fprintf(stderr, "\n\ncalled into a stub library. cannot continue, aborting");
    fprintf(stderr, "this really should not happen. check your dynamic libraries.\n\n");
    abort();
}

XPLM_API int XPLMGetDatavi(XPLMDataRef inDataRef, int * outValues, int inOffset, int inMax) {
    fprintf(stderr, "\n\ncalled into a stub library. cannot continue, aborting");
    fprintf(stderr, "this really should not happen. check your dynamic libraries.\n\n");
    abort();
}

XPLM_API void XPLMSetDatavi(XPLMDataRef inDataRef, int * inValues, int inoffset, int inCount) {
    fprintf(stderr, "\n\ncalled into a stub library. cannot continue, aborting");
    fprintf(stderr, "this really should not happen. check your dynamic libraries.\n\n");
    abort();
}

XPLM_API int XPLMGetDatavf(XPLMDataRef inDataRef, float * outValues, int inOffset, int inMax) {
    fprintf(stderr, "\n\ncalled into a stub library. cannot continue, aborting");
    fprintf(stderr, "this really should not happen. check your dynamic libraries.\n\n");
    abort();
}

XPLM_API void XPLMSetDatavf(XPLMDataRef inDataRef, float * inValues, int inoffset, int inCount) {
    fprintf(stderr, "\n\ncalled into a stub library. cannot continue, aborting");
    fprintf(stderr, "this really should not happen. check your dynamic libraries.\n\n");
    abort();
}

XPLM_API int XPLMGetDatab(XPLMDataRef inDataRef, void * outValue, int inOffset, int inMaxBytes) {
    fprintf(stderr, "\n\ncalled into a stub library. cannot continue, aborting");
    fprintf(stderr, "this really should not happen. check your dynamic libraries.\n\n");
    abort();
}

XPLM_API void XPLMSetDatab(XPLMDataRef inDataRef, void * inValue, int inOffset, int inLength) {
    fprintf(stderr, "\n\ncalled into a stub library. cannot continue, aborting");
    fprintf(stderr, "this really should not happen. check your dynamic libraries.\n\n");
    abort();
}

XPLM_API XPLMDataRef XPLMRegisterDataAccessor(const char * inDataName, XPLMDataTypeID inDataType, int inIsWritable, XPLMGetDatai_f inReadInt, XPLMSetDatai_f inWriteInt, XPLMGetDataf_f inReadFloat, XPLMSetDataf_f inWriteFloat, XPLMGetDatad_f inReadDouble, XPLMSetDatad_f inWriteDouble, XPLMGetDatavi_f inReadIntArray, XPLMSetDatavi_f inWriteIntArray, XPLMGetDatavf_f inReadFloatArray, XPLMSetDatavf_f inWriteFloatArray, XPLMGetDatab_f inReadData, XPLMSetDatab_f inWriteData, void * inReadRefcon, void * inWriteRefcon) {
    fprintf(stderr, "\n\ncalled into a stub library. cannot continue, aborting");
    fprintf(stderr, "this really should not happen. check your dynamic libraries.\n\n");
    abort();
}

XPLM_API void XPLMUnregisterDataAccessor(XPLMDataRef inDataRef) {
    fprintf(stderr, "\n\ncalled into a stub library. cannot continue, aborting");
    fprintf(stderr, "this really should not happen. check your dynamic libraries.\n\n");
    abort();
}

XPLM_API int XPLMShareData(const char * inDataName, XPLMDataTypeID inDataType, XPLMDataChanged_f inNotificationFunc, void * inNotificationRefcon) {
    fprintf(stderr, "\n\ncalled into a stub library. cannot continue, aborting");
    fprintf(stderr, "this really should not happen. check your dynamic libraries.\n\n");
    abort();
}

XPLM_API int XPLMUnshareData(const char * inDataName, XPLMDataTypeID inDataType, XPLMDataChanged_f inNotificationFunc, void * inNotificationRefcon) {
    fprintf(stderr, "\n\ncalled into a stub library. cannot continue, aborting");
    fprintf(stderr, "this really should not happen. check your dynamic libraries.\n\n");
    abort();
}

