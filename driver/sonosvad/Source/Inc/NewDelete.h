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

NewDelete.h

Abstract:

Declaration of placement new and delete operators.
--*/
#pragma once

#ifdef _NEW_DELETE_OPERATORS_

/*****************************************************************************
* Functions
*/

/*****************************************************************************
* ::new()
*****************************************************************************
* New function for creating objects with a specified allocation tag and
* pool type
*/
PVOID operator new
(
    size_t      iSize,
    POOL_FLAGS  poolFlags,
    ULONG       tag
);


/*****************************************************************************
* ::new()
*****************************************************************************
* New function for creating objects with a specified pool type.
*/
PVOID operator new
(
    size_t      iSize,
    POOL_FLAGS  poolFlags
);


/*****************************************************************************
* ::delete()
*****************************************************************************
* Delete with tag function.
*/
void __cdecl operator delete
(
    PVOID pVoid,
    ULONG tag
);


/*****************************************************************************
* ::delete()
*****************************************************************************
* Sized Delete function.
*/
void __cdecl operator delete
(
    _Pre_maybenull_ __drv_freesMem(Mem) PVOID pVoid,
    _In_ size_t cbSize
);


/*****************************************************************************
* ::delete()
*****************************************************************************
* Basic Delete function.
*/
void __cdecl operator delete
(
    PVOID pVoid
);


/*****************************************************************************
* ::delete()
*****************************************************************************
* Sized Array Delete function.
*/
void __cdecl operator delete[]
(
    _Pre_maybenull_ __drv_freesMem(Mem) PVOID pVoid,
    _In_ size_t cbSize
);


/*****************************************************************************
* ::delete()
*****************************************************************************
* Array Delete function.
*/
void __cdecl operator delete[]
(
    _Pre_maybenull_ __drv_freesMem(Mem) PVOID pVoid
);

#endif//_NEW_DELETE_OPERATORS_
