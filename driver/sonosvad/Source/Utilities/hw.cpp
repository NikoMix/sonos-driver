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

Copyright (c)  Microsoft Corporation All Rights Reserved

Module Name:

    hw.cpp

Abstract:

    Implementation of Sonos Virtual Audio Device HW class. 
    Sonos Virtual Audio Device HW has an array for storing mixer and volume settings
    for the topology.
--*/
#include "definitions.h"
#include "hw.h"

//=============================================================================
// CSonosVadHW
//=============================================================================

//=============================================================================
#pragma code_seg("PAGE")
CSonosVadHW::CSonosVadHW()
: m_ulMux(0),
    m_bDevSpecific(FALSE),
    m_iDevSpecific(0),
    m_uiDevSpecific(0)
/*++

Routine Description:

    Constructor for SonosVadHW. 

Arguments:

Return Value:

    void

--*/
{
    PAGED_CODE();
    
    MixerReset();
} // SonosVadHW
#pragma code_seg()


//=============================================================================
BOOL
CSonosVadHW::bGetDevSpecific()
/*++

Routine Description:

  Gets the HW (!) Device Specific info

Arguments:

  N/A

Return Value:

  True or False (in this example).

--*/
{
    return m_bDevSpecific;
} // bGetDevSpecific

//=============================================================================
void
CSonosVadHW::bSetDevSpecific
(
    _In_  BOOL                bDevSpecific
)
/*++

Routine Description:

  Sets the HW (!) Device Specific info

Arguments:

  fDevSpecific - true or false for this example.

Return Value:

    void

--*/
{
    m_bDevSpecific = bDevSpecific;
} // bSetDevSpecific

//=============================================================================
INT
CSonosVadHW::iGetDevSpecific()
/*++

Routine Description:

  Gets the HW (!) Device Specific info

Arguments:

  N/A

Return Value:

  int (in this example).

--*/
{
    return m_iDevSpecific;
} // iGetDevSpecific

//=============================================================================
void
CSonosVadHW::iSetDevSpecific
(
    _In_  INT                 iDevSpecific
)
/*++

Routine Description:

  Sets the HW (!) Device Specific info

Arguments:

  fDevSpecific - true or false for this example.

Return Value:

    void

--*/
{
    m_iDevSpecific = iDevSpecific;
} // iSetDevSpecific

//=============================================================================
UINT
CSonosVadHW::uiGetDevSpecific()
/*++

Routine Description:

  Gets the HW (!) Device Specific info

Arguments:

  N/A

Return Value:

  UINT (in this example).

--*/
{
    return m_uiDevSpecific;
} // uiGetDevSpecific

//=============================================================================
void
CSonosVadHW::uiSetDevSpecific
(
    _In_  UINT                uiDevSpecific
)
/*++

Routine Description:

  Sets the HW (!) Device Specific info

Arguments:

  uiDevSpecific - int for this example.

Return Value:

    void

--*/
{
    m_uiDevSpecific = uiDevSpecific;
} // uiSetDevSpecific


//=============================================================================
BOOL
CSonosVadHW::GetMixerMute
(
    _In_  ULONG                   ulNode,
    _In_  ULONG                   ulChannel
)
/*++

Routine Description:

  Gets the HW (!) mute levels for Sonos Virtual Audio Device

Arguments:

  ulNode - topology node id
  
  ulChannel - which channel are we reading?

Return Value:

  mute setting

--*/
{
    UNREFERENCED_PARAMETER(ulChannel);
    
    if (ulNode < MAX_TOPOLOGY_NODES)
    {
        return m_MuteControls[ulNode];
    }

    return 0;
} // GetMixerMute

//=============================================================================
ULONG                       
CSonosVadHW::GetMixerMux()
/*++

Routine Description:

  Return the current mux selection

Arguments:

Return Value:

  ULONG

--*/
{
    return m_ulMux;
} // GetMixerMux

//=============================================================================
LONG
CSonosVadHW::GetMixerVolume
(   
    _In_  ULONG                   ulNode,
    _In_  ULONG                   ulChannel
)
/*++

Routine Description:

  Gets the HW (!) volume for Sonos Virtual Audio Device.

Arguments:

  ulNode - topology node id

  ulChannel - which channel are we reading?

Return Value:

  LONG - volume level

--*/
{
    UNREFERENCED_PARAMETER(ulChannel);

    if (ulNode < MAX_TOPOLOGY_NODES)
    {
        return m_VolumeControls[ulNode];
    }

    return 0;
} // GetMixerVolume

//=============================================================================
LONG
CSonosVadHW::GetMixerPeakMeter
(   
    _In_  ULONG                   ulNode,
    _In_  ULONG                   ulChannel
)
/*++

Routine Description:

  Gets the HW (!) peak meter for Sonos Virtual Audio Device.

Arguments:

  ulNode - topology node id

  ulChannel - which channel are we reading?

Return Value:

  LONG - sample peak meter level

--*/
{
    UNREFERENCED_PARAMETER(ulChannel);

    if (ulNode < MAX_TOPOLOGY_NODES)
    {
        return m_PeakMeterControls[ulNode];
    }

    return 0;
} // GetMixerVolume

//=============================================================================
#pragma code_seg("PAGE")
void 
CSonosVadHW::MixerReset()
/*++

Routine Description:

  Resets the mixer registers.

Arguments:

Return Value:

    void

--*/
{
    PAGED_CODE();
    
    RtlFillMemory(m_VolumeControls, sizeof(LONG) * MAX_TOPOLOGY_NODES, 0xFF);
    // Endpoints are not muted by default.
    RtlZeroMemory(m_MuteControls, sizeof(BOOL) * MAX_TOPOLOGY_NODES);

    for (ULONG i=0; i<MAX_TOPOLOGY_NODES; ++i)
    {
        m_PeakMeterControls[i] = PEAKMETER_SIGNED_MAXIMUM/2;
    }
    
    // BUGBUG change this depending on the topology
    m_ulMux = 2;
} // MixerReset
#pragma code_seg()

//=============================================================================
void
CSonosVadHW::SetMixerMute
(
    _In_  ULONG                   ulNode,
    _In_  ULONG                   ulChannel,
    _In_  BOOL                    fMute
)
/*++

Routine Description:

  Sets the HW (!) mute levels for Sonos Virtual Audio Device

Arguments:

  ulNode - topology node id

  ulChannel - which channel are we setting?
  
  fMute - mute flag

Return Value:

    void

--*/
{
    UNREFERENCED_PARAMETER(ulChannel);

    if (ulNode < MAX_TOPOLOGY_NODES)
    {
        m_MuteControls[ulNode] = fMute;
    }
} // SetMixerMute

//=============================================================================
void                        
CSonosVadHW::SetMixerMux
(
    _In_  ULONG                   ulNode
)
/*++

Routine Description:

  Sets the HW (!) mux selection

Arguments:

  ulNode - topology node id

Return Value:

    void

--*/
{
    m_ulMux = ulNode;
} // SetMixMux

//=============================================================================
void  
CSonosVadHW::SetMixerVolume
(   
    _In_  ULONG                   ulNode,
    _In_  ULONG                   ulChannel,
    _In_  LONG                    lVolume
)
/*++

Routine Description:

  Sets the HW (!) volume for Sonos Virtual Audio Device.

Arguments:

  ulNode - topology node id

  ulChannel - which channel are we setting?

  lVolume - volume level

Return Value:

    void

--*/
{
    UNREFERENCED_PARAMETER(ulChannel);

    if (ulNode < MAX_TOPOLOGY_NODES)
    {
        m_VolumeControls[ulNode] = lVolume;
    }
} // SetMixerVolume
