#ifndef _XPLMPlatform_h_
#define _XPLMPlatform_h_

#ifdef __linux__
#define LIN 1
#else
#define LIN 0
#endif

#if __APPLE__ && __MACH__
#define APL 1
#else
#define APL 0
#endif

#ifdef _WIN32
#define IBM 1
#else
#define IBM 0
#endif

#if (APL == 0) && (IBM == 0) && (LIN == 0)
#error "Unsupported platform!"
#endif

#ifndef XPLM_LEVEL
#error "How did you even get here? Please set an XPLM compatibility level."
#endif

#if XPLM_LEVEL >= 200
#define XPLM200
#endif

#if XPLM_LEVEL >= 210
#define XPLM210
#endif

#if XPLM_LEVEL >= 300
#define XPLM300
#endif

#if XPLM_LEVEL >= 301
#define XPLM301
#endif

#if XPLM_LEVEL >= 302
#define XPLM302
#endif

#if XPLM_LEVEL >= 303
#define XPLM303
#endif

#if XPLM_LEVEL >= 400
#define XPLM400
#endif

#if defined(__cplusplus) || __STDC_VERSION__ >= 202311L || \
    defined(XPLM_FORCE_ENUM_TYPE)
#define XPLM_ENUM : unsigned int
#define XPLM_ENUM_NAME(it) it
#define XPLM_ENUM_C(_)
#define XPLM_MAYBE_TYPEDEF typedef
#else
#define XPLM_ENUM
#define XPLM_ENUM_NAME(_)
#define XPLM_ENUM_C(it) typedef unsigned int it;
#define XPLM_MAYBE_TYPEDEF
#endif

#endif  // _XPLMPlatform_h_
