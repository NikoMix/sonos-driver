//
// Copyright (c) Microsoft Corporation. All rights reserved.
// Copyright (c) NikoMix and the sonos-driver contributors.
//
// Licensed under the Microsoft Public License (MS-PL).
// See driver/sonosvad/LICENSE-MS-PL.txt for the full licence text.
//
// This file is a derivative work of the "simpleaudiosample" sample from
// https://github.com/microsoft/Windows-driver-samples (MS-PL).
//
/*++

Copyright (c) Microsoft Corporation All Rights Reserved

Module Name:

    hw.h

Abstract:

    Declaration of Sonos Virtual Audio Device HW class. 
    Sonos Virtual Audio Device HW has an array for storing mixer and volume settings
    for the topology.
--*/

#ifndef _SONOSVAD_HW_H_
#define _SONOSVAD_HW_H_

//=============================================================================
// Defines
//=============================================================================
// BUGBUG we should dynamically allocate this...
#define MAX_TOPOLOGY_NODES      20

//=============================================================================
// Classes
//=============================================================================
///////////////////////////////////////////////////////////////////////////////
// CSonosVadHW
// This class represents virtual Sonos Virtual Audio Device HW. An array representing volume
// registers and mute registers.

class CSonosVadHW
{
public:
protected:
    BOOL                        m_MuteControls[MAX_TOPOLOGY_NODES];
    LONG                        m_VolumeControls[MAX_TOPOLOGY_NODES];
    LONG                        m_PeakMeterControls[MAX_TOPOLOGY_NODES];
    ULONG                       m_ulMux;            // Mux selection
    BOOL                        m_bDevSpecific;
    INT                         m_iDevSpecific;
    UINT                        m_uiDevSpecific;

private:

public:
    CSonosVadHW();
    
    void                        MixerReset();
    BOOL                        bGetDevSpecific();
    void                        bSetDevSpecific
    (
        _In_  BOOL                bDevSpecific
    );
    INT                         iGetDevSpecific();
    void                        iSetDevSpecific
    (
        _In_  INT                 iDevSpecific
    );
    UINT                        uiGetDevSpecific();
    void                        uiSetDevSpecific
    (
        _In_  UINT                uiDevSpecific
    );
    BOOL                        GetMixerMute
    (
        _In_  ULONG               ulNode,
        _In_  ULONG               ulChannel
    );
    void                        SetMixerMute
    (
        _In_  ULONG               ulNode,
        _In_  ULONG               ulChannel,
        _In_  BOOL                fMute
    );
    ULONG                       GetMixerMux();
    void                        SetMixerMux
    (
        _In_  ULONG               ulNode
    );
    LONG                        GetMixerVolume
    (   
        _In_  ULONG               ulNode,
        _In_  ULONG               ulChannel
    );
    void                        SetMixerVolume
    (   
        _In_  ULONG               ulNode,
        _In_  ULONG               ulChannel,
        _In_  LONG                lVolume
    );
    
    LONG                        GetMixerPeakMeter
    (   
        _In_  ULONG               ulNode,
        _In_  ULONG               ulChannel
    );

protected:
private:
};
typedef CSonosVadHW    *PCSonosVadHW;

#endif  // _SONOSVAD_HW_H_
