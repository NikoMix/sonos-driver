/*++

Copyright (c) Microsoft Corporation All Rights Reserved
Copyright (c) NikoMix (sonos-driver contributors)

Licensed under the Microsoft Public License (MS-PL).
See LICENSE-MS-PL.txt in the sonosvad directory.

Derived from the Windows-driver-samples "sonosvad" sample.

Module Name:

    minipairs.h

Abstract:

    Local audio endpoint filter definitions.

    sonosvad declares SONOSVAD_ENDPOINT_COUNT identical render endpoints. Each
    one is a null sink that the user-mode service binds to a discovered Sonos
    room and taps via WASAPI loopback. There are no capture endpoints.

--*/

#ifndef _SONOSVAD_MINIPAIRS_H_
#define _SONOSVAD_MINIPAIRS_H_

#include "speakertopo.h"
#include "speakertoptable.h"
#include "speakerwavtable.h"

NTSTATUS
CreateMiniportWaveRTSonosVad
( 
    _Out_       PUNKNOWN *,
    _In_        REFCLSID,
    _In_opt_    PUNKNOWN,
    _In_        POOL_FLAGS,
    _In_        PUNKNOWN,
    _In_opt_    PVOID,
    _In_        PENDPOINT_MINIPAIR
);

NTSTATUS
CreateMiniportTopologySonosVad
( 
    _Out_       PUNKNOWN *,
    _In_        REFCLSID,
    _In_opt_    PUNKNOWN,
    _In_        POOL_FLAGS,
    _In_        PUNKNOWN,
    _In_opt_    PVOID,
    _In_        PENDPOINT_MINIPAIR
);

//
// Render miniports.
//

/*********************************************************************
* Topology/Wave bridge connection for speaker (internal)             *
*                                                                    *
*              +------+                +------+                      *
*              | Wave |                | Topo |                      *
*              |      |                |      |                      *
* System   --->|0    1|--------------->|0    1|---> Line Out         *
*              |      |                |      |                      *
*              +------+                +------+                      *
*********************************************************************/
static
PHYSICALCONNECTIONTABLE SpeakerTopologyPhysicalConnections[] =
{
    {
        KSPIN_TOPO_WAVEOUT_SOURCE,  // TopologyIn
        KSPIN_WAVE_RENDER3_SOURCE,  // WaveOut
        CONNECTIONTYPE_WAVE_OUTPUT
    }
};

//
// One ENDPOINT_MINIPAIR per virtual output.
//
// The KS reference strings below MUST match the KSNAME_* entries in the INF's
// [Strings] section and the AddInterface directives in
// [SonosVad.I.Interfaces]. Keep sonosvad.inx in sync when changing
// SONOSVAD_ENDPOINT_COUNT.
//
#define SONOSVAD_ENDPOINT_MINIPAIR(n)                                       \
static                                                                      \
ENDPOINT_MINIPAIR SonosOutput##n##Miniports =                               \
{                                                                           \
    eSonosOutputDevice,                                                     \
    L"TopologySonosOutput" #n,  /* matches KSNAME_TopologySonosOutput<n> */ \
    NULL,                       /* optional template name                */ \
    CreateMiniportTopologySonosVad,                                         \
    &SpeakerTopoMiniportFilterDescriptor,                                   \
    0, NULL,                    /* Interface properties                  */ \
    L"WaveSonosOutput" #n,      /* matches KSNAME_WaveSonosOutput<n>     */ \
    NULL,                       /* optional template name                */ \
    CreateMiniportWaveRTSonosVad,                                           \
    &SpeakerWaveMiniportFilterDescriptor,                                   \
    0,                          /* Interface properties                  */ \
    NULL,                                                                   \
    SPEAKER_DEVICE_MAX_CHANNELS,                                            \
    SpeakerPinDeviceFormatsAndModes,                                        \
    SIZEOF_ARRAY(SpeakerPinDeviceFormatsAndModes),                          \
    SpeakerTopologyPhysicalConnections,                                     \
    SIZEOF_ARRAY(SpeakerTopologyPhysicalConnections),                       \
    ENDPOINT_NO_FLAGS,                                                      \
};

SONOSVAD_ENDPOINT_MINIPAIR(1)
SONOSVAD_ENDPOINT_MINIPAIR(2)
SONOSVAD_ENDPOINT_MINIPAIR(3)
SONOSVAD_ENDPOINT_MINIPAIR(4)
SONOSVAD_ENDPOINT_MINIPAIR(5)
SONOSVAD_ENDPOINT_MINIPAIR(6)
SONOSVAD_ENDPOINT_MINIPAIR(7)
SONOSVAD_ENDPOINT_MINIPAIR(8)
SONOSVAD_ENDPOINT_MINIPAIR(9)

//=============================================================================
//
// Render miniport pairs.
//
static
PENDPOINT_MINIPAIR  g_RenderEndpoints[] = 
{
    &SonosOutput1Miniports,
    &SonosOutput2Miniports,
    &SonosOutput3Miniports,
    &SonosOutput4Miniports,
    &SonosOutput5Miniports,
    &SonosOutput6Miniports,
    &SonosOutput7Miniports,
    &SonosOutput8Miniports,
    &SonosOutput9Miniports,
};

#define g_cRenderEndpoints  (SIZEOF_ARRAY(g_RenderEndpoints))

//=============================================================================
//
// sonosvad exposes no capture endpoints. System audio reaches user mode
// through the audio engine's WASAPI loopback tap on each render endpoint.
//
#define g_cCaptureEndpoints (0)

//=============================================================================
//
// Total miniports = # endpoints * 2 (topology + wave).
//
#define g_MaxMiniports  ((g_cRenderEndpoints + g_cCaptureEndpoints) * 2)

C_ASSERT(g_cRenderEndpoints == SONOSVAD_ENDPOINT_COUNT);

#endif // _SONOSVAD_MINIPAIRS_H_
