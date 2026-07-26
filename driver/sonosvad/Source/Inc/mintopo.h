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

    minitopo.h

Abstract:

    Declaration of topology miniport.
--*/

#ifndef _SONOSVAD_MINTOPO_H_
#define _SONOSVAD_MINTOPO_H_

#include "basetopo.h"

//=============================================================================
// Classes
//=============================================================================

///////////////////////////////////////////////////////////////////////////////
// CMiniportTopology 
//   

class CMiniportTopology : 
    public CMiniportTopologySonosVad,
    public IMiniportTopology,
    public CUnknown
{
  private:
    eDeviceType             m_DeviceType;

    union {
        PVOID               m_DeviceContext;
    };

public:
    DECLARE_STD_UNKNOWN();
    CMiniportTopology
    (
        _In_opt_    PUNKNOWN                UnknownOuter,
        _In_        PCFILTER_DESCRIPTOR    *FilterDesc,
        _In_        USHORT                  DeviceMaxChannels,
        _In_        eDeviceType             DeviceType, 
        _In_opt_    PVOID                   DeviceContext
    )
    : CUnknown(UnknownOuter),
      CMiniportTopologySonosVad(FilterDesc, DeviceMaxChannels),
      m_DeviceType(DeviceType),
      m_DeviceContext(DeviceContext)
    {
    }

    ~CMiniportTopology();

    IMP_IMiniportTopology;

    NTSTATUS PropertyHandlerJackDescription
    (
        _In_        PPCPROPERTY_REQUEST                         PropertyRequest,
        _In_        ULONG                                       cJackDescriptions,
        _In_reads_(cJackDescriptions) PKSJACK_DESCRIPTION       *JackDescriptions
    );

    NTSTATUS PropertyHandlerJackDescription2
    ( 
        _In_        PPCPROPERTY_REQUEST                         PropertyRequest,
        _In_        ULONG                                       cJackDescriptions,
        _In_reads_(cJackDescriptions) PKSJACK_DESCRIPTION       *JackDescriptions,
        _In_        DWORD                                       JackCapabilities
    );
    
    PVOID GetDeviceContext() { return m_DeviceContext;  }
};

typedef CMiniportTopology *PCMiniportTopology;

#endif // _SONOSVAD_MINTOPO_H_
