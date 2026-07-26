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

    minwavert.h

Abstract:

    Definition of wavert miniport class.
--*/

#ifndef _SONOSVAD_MINWAVERT_H_
#define _SONOSVAD_MINWAVERT_H_

//=============================================================================
// Referenced Forward
//=============================================================================
class CMiniportWaveRTStream;
typedef CMiniportWaveRTStream *PCMiniportWaveRTStream;

//=============================================================================
// Classes
//=============================================================================
///////////////////////////////////////////////////////////////////////////////
// CMiniportWaveRT
//   
class CMiniportWaveRT : 
    public IMiniportWaveRT,
    public IMiniportAudioSignalProcessing,
    public CUnknown
{
private:
    //
    // In-class initialisers: the destructor releases several of these, and
    // construction can be followed by a failed Init(), so none of them may
    // ever be observed uninitialised.
    //
    ULONG                               m_ulSystemAllocated = 0;

    ULONG                               m_ulMaxSystemStreams = 0;
    ULONG                               m_ulMaxOffloadStreams = 0;
    ULONG                               m_ulMaxLoopbackStreams = 0;

    // weak ref of running streams.
    PCMiniportWaveRTStream            * m_SystemStreams = nullptr;

    BOOL                                m_bGfxEnabled = FALSE;
    PBOOL                               m_pbMuted = nullptr;
    PLONG                               m_plVolumeLevel = nullptr;
    PKSDATAFORMAT_WAVEFORMATEXTENSIBLE  m_pMixFormat = nullptr;
    PKSDATAFORMAT_WAVEFORMATEXTENSIBLE  m_pDeviceFormat = nullptr;
    PCFILTER_DESCRIPTOR                 m_FilterDesc{};
    PIN_DEVICE_FORMATS_AND_MODES *      m_DeviceFormatsAndModes = nullptr;
    KSPIN_LOCK                          m_DeviceFormatsAndModesLock = 0; // To serialize access.
    KIRQL                               m_DeviceFormatsAndModesIrql = 0;
    ULONG                               m_DeviceFormatsAndModesCount = 0;
    USHORT                              m_DeviceMaxChannels = 0;
    PDRMPORT                            m_pDrmPort = nullptr;
    DRMRIGHTS                           m_MixDrmRights{};
    ULONG                               m_ulMixDrmContentId = 0;

    union {
        PVOID                           m_DeviceContext;
    };

protected:
    PADAPTERCOMMON                      m_pAdapterCommon = nullptr;
    ULONG                               m_DeviceFlags = 0;
    eDeviceType                         m_DeviceType = eSonosOutputDevice;
    PPORTEVENTS                         m_pPortEvents = nullptr;
    PENDPOINT_MINIPAIR                  m_pMiniportPair = nullptr;
    
public:
    NTSTATUS EventHandler_PinCapsChange
    (
        _In_  PPCEVENT_REQUEST EventRequest
    );

    NTSTATUS ValidateStreamCreate
    (
        _In_    ULONG   _Pin,
        _In_    BOOLEAN _Capture
    );
    
    NTSTATUS StreamCreated
    (
        _In_ ULONG                  _Pin,
        _In_ PCMiniportWaveRTStream _Stream
    );
    
    NTSTATUS StreamClosed
    (
        _In_ ULONG                  _Pin,
        _In_ PCMiniportWaveRTStream _Stream
    );
    
    NTSTATUS IsFormatSupported
    ( 
        _In_ ULONG          _ulPin,
        _In_ BOOLEAN        _bCapture,
        _In_ PKSDATAFORMAT  _pDataFormat
    );

    static NTSTATUS GetAttributesFromAttributeList
    (
        _In_ const KSMULTIPLE_ITEM *_pAttributes,
        _In_ size_t _Size,
        _Out_ GUID* _pSignalProcessingMode
    );

protected:
    NTSTATUS UpdateDrmRights
    (
        void
    );

public:
    DECLARE_STD_UNKNOWN();

#pragma code_seg("PAGE")
    CMiniportWaveRT(
        _In_            PUNKNOWN                                UnknownAdapter,
        _In_            PENDPOINT_MINIPAIR                      MiniportPair,
        _In_opt_        PVOID                                   DeviceContext
    )
        :CUnknown(0),
        m_ulMaxSystemStreams(0),
        m_DeviceType(MiniportPair->DeviceType),
        m_DeviceContext(DeviceContext),
        m_DeviceMaxChannels(MiniportPair->DeviceMaxChannels),
        m_DeviceFormatsAndModes(MiniportPair->PinDeviceFormatsAndModes),
        m_DeviceFormatsAndModesCount(MiniportPair->PinDeviceFormatsAndModesCount),
        m_DeviceFlags(MiniportPair->DeviceFlags),
        m_pMiniportPair(MiniportPair)
    {
        PAGED_CODE();

        m_pAdapterCommon = (PADAPTERCOMMON)UnknownAdapter; // weak ref.

        if (MiniportPair->WaveDescriptor)
        {
            RtlCopyMemory(&m_FilterDesc, MiniportPair->WaveDescriptor, sizeof(m_FilterDesc));
            
            //
            // Get the max # of pin instances.
            //
            if (IsRenderDevice())
            {
                if (m_FilterDesc.PinCount > KSPIN_WAVE_RENDER2_SOURCE)
                {
                    m_ulMaxSystemStreams = m_FilterDesc.Pins[KSPIN_WAVE_RENDER2_SINK_SYSTEM].MaxFilterInstanceCount;
                    m_ulMaxLoopbackStreams = m_FilterDesc.Pins[KSPIN_WAVE_RENDER2_SINK_LOOPBACK].MaxFilterInstanceCount;
                }
                else if(m_FilterDesc.PinCount > KSPIN_WAVE_RENDER3_SOURCE)
                {
                    m_ulMaxSystemStreams = m_FilterDesc.Pins[KSPIN_WAVE_RENDER3_SINK_SYSTEM].MaxFilterInstanceCount;
                }
            }
            else
            {
                //
                // capture bridge pin comes first in the enumeration
                //
                if (m_FilterDesc.PinCount > KSPIN_WAVEIN_HOST)
                {
                    m_ulMaxSystemStreams = m_FilterDesc.Pins[KSPIN_WAVEIN_HOST].MaxFilterInstanceCount;
                }
            }
        }

        KeInitializeSpinLock(&m_DeviceFormatsAndModesLock);
        m_DeviceFormatsAndModesIrql = PASSIVE_LEVEL;
    }

#pragma code_seg()

    ~CMiniportWaveRT();

    IMP_IMiniportWaveRT;
    IMP_IMiniportAudioSignalProcessing;
    
    // Friends
    friend class        CMiniportWaveRTStream;
    friend class        CMiniportTopologySonosVad;
    
    friend NTSTATUS PropertyHandler_WaveFilter
    (   
        _In_ PPCPROPERTY_REQUEST      PropertyRequest 
    );   

public:
public:
    NTSTATUS PropertyHandlerProposedFormat
    (
        _In_ PPCPROPERTY_REQUEST PropertyRequest
    );

    NTSTATUS PropertyHandlerProposedFormat2

    (
        _In_ PPCPROPERTY_REQUEST PropertyRequest
    );

    PADAPTERCOMMON GetAdapterCommObj() 
    {
        return m_pAdapterCommon; 
    };
#pragma code_seg()

private:
    _IRQL_raises_(DISPATCH_LEVEL)
    _Acquires_lock_(m_DeviceFormatsAndModesLock)
    _Requires_lock_not_held_(m_DeviceFormatsAndModesLock)
    _IRQL_saves_global_(SpinLock, m_DeviceFormatsAndModesIrql)
    VOID AcquireFormatsAndModesLock();

    _Releases_lock_(m_DeviceFormatsAndModesLock)
    _Requires_lock_held_(m_DeviceFormatsAndModesLock)
    _IRQL_restores_global_(SpinLock, m_DeviceFormatsAndModesIrql)
    VOID ReleaseFormatsAndModesLock();

    _Post_satisfies_(return > 0)
    ULONG GetPinSupportedDeviceFormats(_In_ ULONG PinId, _Outptr_opt_result_buffer_(return) KSDATAFORMAT_WAVEFORMATEXTENSIBLE **ppFormats);

    _Success_(return != 0)
    ULONG GetPinSupportedDeviceModes(_In_ ULONG PinId, _Outptr_opt_result_buffer_(return) _On_failure_(_Deref_post_null_) MODE_AND_DEFAULT_FORMAT **ppModes);

#pragma code_seg()

protected:
#pragma code_seg()
    BOOL IsRenderDevice()
    {
        return m_DeviceType == eSonosOutputDevice ? TRUE : FALSE;
    }

    BOOL IsSystemRenderPin(ULONG nPinId);

    BOOL IsSystemCapturePin(ULONG nPinId);

    BOOL IsBridgePin(ULONG nPinId);

    // These three pins are the pins used by the audio engine for host, loopback, and offload.
    ULONG GetSystemPinId()
    {
        ASSERT(IsRenderDevice());
        return KSPIN_WAVE_RENDER2_SINK_SYSTEM;
    }
};

typedef CMiniportWaveRT *PCMiniportWaveRT;

#endif // _SONOSVAD_MINWAVERT_H_
