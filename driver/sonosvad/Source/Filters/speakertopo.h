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

    speakertopo.h

Abstract:

    Declaration of topology miniport for the speaker (internal).
--*/

#ifndef _SONOSVAD_SPEAKERTOPO_H_
#define _SONOSVAD_SPEAKERTOPO_H_

NTSTATUS PropertyHandler_SpeakerTopoFilter(_In_ PPCPROPERTY_REQUEST PropertyRequest);

NTSTATUS PropertyHandler_SpeakerTopology(_In_ PPCPROPERTY_REQUEST PropertyRequest);

#endif // _SONOSVAD_SPEAKERTOPO_H_
