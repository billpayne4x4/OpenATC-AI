// SPDX-FileCopyrightText: 2024 Julia DeMille <me@jdemille.com
//
// SPDX-License-Identifier: Apache-2.0 OR MIT

/*
 * This file includes all the XPLM and XPWidgets headers
 */

#ifdef _FMOD_STUB_
#define _FMOD_COMMON_H
// Basic definitions to make bindings possible.
typedef void FMOD_STUDIO_SYSTEM;
typedef void FMOD_CHANNELGROUP;
typedef int FMOD_RESULT;
typedef int FMOD_SOUND_FORMAT;
typedef void FMOD_CHANNEL;
typedef void FMOD_VECTOR;
#endif

#include <XPStandardWidgets.h>
#include <XPUIGraphics.h>
#include <XPWidgetDefs.h>
#include <XPWidgets.h>
#include <XPWidgetUtils.h>

#include <XPLMCamera.h>
#include <XPLMDataAccess.h>
#include <XPLMDefs.h>
#include <XPLMDisplay.h>
#include <XPLMGraphics.h>
#include <XPLMInstance.h>
#include <XPLMMap.h>
#include <XPLMMenus.h>
#include <XPLMNavigation.h>
#include <XPLMPlanes.h>
#include <XPLMPlugin.h>
#include <XPLMProcessing.h>
#include <XPLMScenery.h>
#include <XPLMSound.h>
#include <XPLMUtilities.h>
#include <XPLMWeather.h>
