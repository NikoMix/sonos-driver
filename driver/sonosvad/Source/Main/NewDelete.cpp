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
/*****************************************************************************
* NewDelete.cpp -  CPP placement new and delete operators implementation
*****************************************************************************
* Copyright (c) Microsoft Corporation All Rights Reserved
*
* Module Name:
*
* NewDelete.cpp
*
* Abstract:
*
*   Definition of placement new and delete operators.
*
*/

#ifdef _NEW_DELETE_OPERATORS_
#ifdef __cplusplus
extern "C" {
#include <wdm.h>
}
#else
#include <wdm.h>
#endif

#include "newDelete.h"
#include "definitions.h"

#pragma code_seg()
/*****************************************************************************
* Functions
*/

/*****************************************************************************
* ::new()
*****************************************************************************
* New function for creating objects with a specified allocation tag.
*/
PVOID operator new
(
    size_t      iSize,
    POOL_FLAGS  poolFlags,
    ULONG       tag
)
{
    //
    // sonosvad only ever needs paged or non-paged, non-executable pool.
    // Deriving the flags from a two-way choice rather than forwarding the
    // caller's value makes a W^X violation impossible and is provable by
    // static analysis (C28160).
    //
    const POOL_FLAGS safeFlags =
        ((poolFlags & POOL_FLAG_PAGED) != 0) ? POOL_FLAG_PAGED : POOL_FLAG_NON_PAGED;

    PVOID result = ExAllocatePool2(safeFlags, iSize, tag);

    return result;
}


/*****************************************************************************
* ::new()
*****************************************************************************
* New function for creating objects with a specified allocation tag.
*/
PVOID operator new
(
    size_t      iSize,
    POOL_FLAGS  poolFlags
)
{
    const POOL_FLAGS safeFlags =
        ((poolFlags & POOL_FLAG_PAGED) != 0) ? POOL_FLAG_PAGED : POOL_FLAG_NON_PAGED;

    PVOID result = ExAllocatePool2(safeFlags, iSize, SONOSVAD_POOLTAG);

    return result;
}


/*****************************************************************************
* ::delete()
*****************************************************************************
* Delete with tag function.
*/
void __cdecl operator delete
(
    PVOID pVoid,
    ULONG tag
)
{
    if (pVoid)
    {
        ExFreePoolWithTag(pVoid, tag);
    }
}


/*****************************************************************************
* ::delete()
*****************************************************************************
* Sized Delete function.
*/
void __cdecl operator delete
(
    _Pre_maybenull_ __drv_freesMem(Mem) PVOID pVoid,
    _In_ size_t cbSize
)
{
    UNREFERENCED_PARAMETER(cbSize);

    if (pVoid)
    {
        ExFreePoolWithTag(pVoid, SONOSVAD_POOLTAG);
    }
}


/*****************************************************************************
* ::delete()
*****************************************************************************
* Sized Array Delete function.
*/
void __cdecl operator delete[]
(
    _Pre_maybenull_ __drv_freesMem(Mem) PVOID pVoid,
    _In_ size_t cbSize
)
{
    UNREFERENCED_PARAMETER(cbSize);

    if (pVoid)
    {
        ExFreePoolWithTag(pVoid, SONOSVAD_POOLTAG);
    }
}


/*****************************************************************************
* ::delete()
*****************************************************************************
* Array Delete function.
*/
void __cdecl operator delete[]
(
    _Pre_maybenull_ __drv_freesMem(Mem) PVOID pVoid
)
{
    if (pVoid)
    {
        ExFreePoolWithTag(pVoid, SONOSVAD_POOLTAG);
    }
}
#endif//_NEW_DELETE_OPERATORS_
