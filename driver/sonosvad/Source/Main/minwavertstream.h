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

    minwavertstream.h

Abstract:

    Definition of wavert miniport class.
--*/

#ifndef _SONOSVAD_MINWAVERTSTREAM_H_
#define _SONOSVAD_MINWAVERTSTREAM_H_

//
// Structure to store notifications events in a protected list
//
typedef struct _NotificationListEntry
{
    LIST_ENTRY  ListEntry;
    PKEVENT     NotificationEvent;
} NotificationListEntry;

EXT_CALLBACK   TimerNotifyRT;

//=============================================================================
// Referenced Forward
//=============================================================================
class CMiniportWaveRT;
typedef CMiniportWaveRT *PCMiniportWaveRT;

//=============================================================================
// Classes
//=============================================================================
///////////////////////////////////////////////////////////////////////////////
// CMiniportWaveRTStream 
// 
class CMiniportWaveRTStream : 
    public IDrmAudioStream,
    public IMiniportWaveRTStreamNotification,
    public IMiniportWaveRTInputStream,
    public IMiniportWaveRTOutputStream,
    public CUnknown
{
protected:
    //
    // Every member is initialized in-class: the object is constructed with
    // DEFINE_STD_CONSTRUCTOR and only populated by Init(). If Init() fails
    // partway through, the destructor still runs and frees several of these
    // pointers, so they must never hold garbage.
    //
    PPORTWAVERTSTREAM           m_pPortStream = nullptr;
    LIST_ENTRY                  m_NotificationList{};
    PEX_TIMER                   m_pNotificationTimer = nullptr;
    ULONG                       m_ulNotificationIntervalMs = 0;
    ULONG                       m_ulCurrentWritePosition = 0;
    LONG                        m_IsCurrentWritePositionUpdated = 0;
    
public:
    DECLARE_STD_UNKNOWN();
    DEFINE_STD_CONSTRUCTOR(CMiniportWaveRTStream);
    ~CMiniportWaveRTStream();

    IMP_IMiniportWaveRTStream;
    IMP_IMiniportWaveRTStreamNotification;
    IMP_IMiniportWaveRTInputStream;
    IMP_IMiniportWaveRTOutputStream;
    IMP_IMiniportWaveRT;
    IMP_IDrmAudioStream;

    NTSTATUS                    Init
    ( 
        _In_  PCMiniportWaveRT    Miniport,
        _In_  PPORTWAVERTSTREAM   Stream,
        _In_  ULONG               Channel,
        _In_  BOOLEAN             Capture,
        _In_  PKSDATAFORMAT       DataFormat,
        _In_  GUID                SignalProcessingMode
    );

    // Friends
    friend class                CMiniportWaveRT;
    friend EXT_CALLBACK         TimerNotifyRT;
protected:
    CMiniportWaveRT*            m_pMiniport = nullptr;
    ULONG                       m_ulPin = 0;
    BOOLEAN                     m_bCapture = FALSE;
    BOOLEAN                     m_bUnregisterStream = FALSE;
    ULONG                       m_ulDmaBufferSize = 0;
    BYTE*                       m_pDmaBuffer = nullptr;
    ULONG                       m_ulNotificationsPerBuffer = 0;
    KSSTATE                     m_KsState = KSSTATE_STOP;
    PKTIMER                     m_pTimer = nullptr;
    PRKDPC                      m_pDpc = nullptr;
    ULONGLONG                   m_ullPlayPosition = 0;
    ULONGLONG                   m_ullWritePosition = 0;
    ULONGLONG                   m_ullLinearPosition = 0;
    ULONGLONG                   m_ullPresentationPosition = 0;
    ULONG                       m_ulLastOsReadPacket = 0;
    ULONG                       m_ulLastOsWritePacket = 0;
    LONGLONG                    m_llPacketCounter = 0;
    ULONGLONG                   m_ullDmaTimeStamp = 0;
    LARGE_INTEGER               m_ullPerformanceCounterFrequency{};
    ULONGLONG                   m_hnsElapsedTimeCarryForward = 0;
    ULONGLONG                   m_ullLastDPCTimeStamp = 0;
    ULONGLONG                   m_hnsDPCTimeCarryForward = 0;
    ULONG                       m_byteDisplacementCarryForward = 0;
    ULONG                       m_ulDmaMovementRate = 0;
    BOOL                        m_bLfxEnabled = FALSE;
    PBOOL                       m_pbMuted = nullptr;
    PLONG                       m_plVolumeLevel = nullptr;
    PLONG                       m_plPeakMeter = nullptr;
    PWAVEFORMATEXTENSIBLE       m_pWfExt = nullptr;
    ULONG                       m_ulContentId = 0;
    GUID                        m_SignalProcessingMode{};
    BOOLEAN                     m_bEoSReceived = FALSE;
    BOOLEAN                     m_bLastBufferRendered = FALSE;
    KSPIN_LOCK                  m_PositionSpinLock = 0;

public:

    //presentation
    NTSTATUS GetPresentationPosition
    (
        _Out_  KSAUDIO_PRESENTATION_POSITION *_pPresentationPosition
    );
        
    ULONG GetCurrentWaveRTWritePosition() 
    {
        return m_ulCurrentWritePosition;
    };

    // To support simple underrun validation.
    BOOL IsCurrentWaveRTWritePositionUpdated() 
    {
        return InterlockedExchange(&m_IsCurrentWritePositionUpdated, 0) ? TRUE : FALSE;
    };

    GUID GetSignalProcessingMode()
    {
        return m_SignalProcessingMode;
    }
    
private:

    //
    // Helper functions.
    //
    
#pragma code_seg()

    VOID WriteBytes
    (
        _In_ ULONG ByteDisplacement
    );

    VOID UpdatePosition
    (
        _In_ LARGE_INTEGER ilQPC
    );
    
    NTSTATUS SetCurrentWritePositionInternal
    (
        _In_  ULONG ulCurrentWritePosition
    );
    
    NTSTATUS GetPositions
    (
        _Out_opt_  ULONGLONG *      _pullLinearBufferPosition, 
        _Out_opt_  ULONGLONG *      _pullPresentationPosition, 
        _Out_opt_  LARGE_INTEGER *  _pliQPCTime
    );

};
typedef CMiniportWaveRTStream *PCMiniportWaveRTStream;
#endif // _SONOSVAD_MINWAVERTSTREAM_H_

