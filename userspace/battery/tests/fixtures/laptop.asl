/*
 * A laptop firmware table written for the OSyx battery tests. It has two
 * batteries implemented the way two common firmware families do it:
 *
 *   BAT0: capacities in mAh converted to mWh by the firmware, its embedded
 *         controller RAM memory-mapped (SystemMemory, fields at odd
 *         addresses), status built by helper methods with many arguments.
 *   BAT1: packages of packages indexed by the battery number and filled
 *         through DerefOf (Index ()) targets, a battery select register,
 *         Switch statements, strings built with Concatenate, ToDecimalString,
 *         Mid and ToString, buffer fields.
 *
 * Around them: an IndexField (CMOS), a PCI_Config region, _OSI, CondRefOf,
 * Match, FindSetRightBit, Timer, RefOf. Not copied from any vendor's table.
 * Compile with: iasl laptop.asl
 */
DefinitionBlock ("", "DSDT", 2, "OSYX  ", "LAPTOP  ", 0x00000001)
{
    Name (OSYS, 0x07D0)
    Name (ECFL, Zero)
    Name (TMRS, Zero)

    OperationRegion (DBG0, SystemIO, 0x80, One)
    Field (DBG0, ByteAcc, NoLock, Preserve)
    {
        P80H,   8
    }

    OperationRegion (CMS0, SystemIO, 0x72, 0x02)
    Field (CMS0, ByteAcc, NoLock, Preserve)
    {
        CMI0,   8,
        CMD0,   8
    }

    IndexField (CMI0, CMD0, ByteAcc, NoLock, Preserve)
    {
        Offset (0x40),
        BCYC,   16,
        BFLG,   4,
        BLVL,   4
    }

    Scope (\_SB)
    {
        Method (_INI, 0, NotSerialized)
        {
            If (CondRefOf (\_OSI, Local0))
            {
                If (\_OSI ("Windows 2015"))
                {
                    OSYS = 0x07DF
                }
            }

            P80H = 0x55
            TMRS = Timer
        }

        Device (PCI0)
        {
            Name (_HID, EisaId ("PNP0A08"))
            Name (_CID, EisaId ("PNP0A03"))
            Name (_ADR, Zero)
            Name (_PRT, Package (0x02)
            {
                Package (0x04) { 0x0001FFFF, Zero, Zero, 0x10 },
                Package (0x04) { 0x0001FFFF, One, Zero, 0x11 }
            })

            Device (LPCB)
            {
                Name (_ADR, 0x001F0000)
                OperationRegion (LPC0, PCI_Config, 0x40, 0xC0)
                Field (LPC0, AnyAcc, NoLock, Preserve)
                {
                    Offset (0x40),
                    IOD0,   8
                }

                Device (EC0)
                {
                    Name (_HID, EisaId ("PNP0C09"))
                    Name (_UID, One)
                    Name (_GPE, 0x17)
                    Name (_CRS, ResourceTemplate ()
                    {
                        IO (Decode16, 0x0062, 0x0062, 0x00, 0x01, )
                        IO (Decode16, 0x0066, 0x0066, 0x00, 0x01, )
                    })
                    Method (_REG, 2, NotSerialized)
                    {
                        If ((Arg0 == 0x03))
                        {
                            ECFL = Arg1
                        }
                    }

                    /* Memory-mapped EC RAM: BAT0 lives here. */
                    OperationRegion (ERAM, SystemMemory, 0xFE0B0300, 0x0100)
                    Field (ERAM, ByteAcc, Lock, Preserve)
                    {
                        Offset (0x04),
                        CMD1,   8,
                        CDT1,   8,
                        Offset (0x93),
                        TAH0,   16,
                        Offset (0xB1),
                        B0DC,   16,
                        B0FC,   16,
                        B0RC,   16,
                        B0DV,   16,
                        B0PV,   16,
                        B0CR,   16,
                        B0MD,   16,
                        Offset (0xC1),
                        B0PR,   1,
                        B0CH,   1,
                        B0DI,   1,
                            ,   4,
                        ACPR,   1
                    }

                    /* The EC proper: BAT1 lives here, behind a select register. */
                    OperationRegion (ECRG, EmbeddedControl, Zero, 0x0100)
                    Field (ECRG, ByteAcc, NoLock, Preserve)
                    {
                        Offset (0x10),
                        NGBF,   8,
                        NDBS,   8,
                        Offset (0x20),
                        BSEL,   4,
                        Offset (0x21),
                        BSTS,   8,
                        BDCC,   16,
                        BFCC,   16,
                        BRCC,   16,
                        BDVO,   16,
                        BPVO,   16,
                        BCRN,   16,
                        BSNO,   16,
                        BDAT,   16,
                        Offset (0x40),
                        BCTL,   128
                    }

                    Mutex (MUEC, 0x00)
                    Mutex (ECMX, 0x00)

                    Method (ECAV, 0, NotSerialized)
                    {
                        If ((ECFL >= One))
                        {
                            Return (One)
                        }

                        Return (Zero)
                    }

                    Method (BATP, 1, Serialized)
                    {
                        If (!ECAV ())
                        {
                            Return (Zero)
                        }

                        Switch (ToInteger (Arg0))
                        {
                            Case (Zero)
                            {
                                Return (B0PR)
                            }
                            Case (One)
                            {
                                Return ((BSTS & One))
                            }
                            Default
                            {
                                Return (Zero)
                            }
                        }
                    }

                    Method (ACAP, 0, Serialized)
                    {
                        Return (ACPR)
                    }

                    Method (BIF0, 0, NotSerialized)
                    {
                        If (ECAV ())
                        {
                            Acquire (MUEC, 0xFFFF)
                            Local0 = B0MD
                            Release (MUEC)
                            If ((Local0 != 0xFFFF))
                            {
                                Local1 = (Local0 >> 0x0F)
                                Local1 &= One
                                Local0 = (Local1 ^ One)
                            }
                        }
                        Else
                        {
                            Local0 = Ones
                        }

                        Return (Local0)
                    }

                    Method (BIF1, 0, NotSerialized) { If (ECAV ()) { Return (B0DC) } Return (Ones) }
                    Method (BIF2, 0, NotSerialized) { If (ECAV ()) { Return (B0FC) } Return (Ones) }
                    Method (BIF4, 0, NotSerialized) { If (ECAV ()) { Return (B0DV) } Return (Ones) }
                    Method (B0SS, 0, NotSerialized) { Return ((B0CH | (B0DI << One))) }
                    Method (BCRT, 0, NotSerialized) { Return (B0CR) }
                    Method (BRCP, 0, NotSerialized) { Return (B0RC) }
                    Method (BVOL, 0, NotSerialized) { Return (B0PV) }

                    /* BAT1: packages of packages, one per battery slot. */
                    Name (NBTI, Package (0x02)
                    {
                        Package (0x0D)
                        {
                            One, 0xFFFFFFFF, 0xFFFFFFFF, One, 0xFFFFFFFF, Zero, Zero, 0x64, 0x64, "Primary", "100000", "LIon", "OSyx"
                        },

                        Package (0x0D)
                        {
                            One, 0xFFFFFFFF, 0xFFFFFFFF, One, 0xFFFFFFFF, Zero, Zero, 0x64, 0x64, "Travel", "100000", "LIon", "OSyx"
                        }
                    })
                    Name (NBST, Package (0x02)
                    {
                        Package (0x04) { Zero, Zero, 0x0FA0, 0x04B0 },
                        Package (0x04) { Zero, Zero, 0x0FA0, 0x04B0 }
                    })
                    Name (NLB1, 0x0A)
                    Name (NLB2, 0x05)

                    Method (GBSS, 2, Serialized)
                    {
                        /* "nnnnn dd/mm/yyyy" from a serial number and a packed date. */
                        Name (GBUF, Buffer (0x06) {})
                        CreateWordField (GBUF, Zero, GSER)
                        CreateByteField (GBUF, 0x02, GDAY)
                        GSER = Arg0
                        GDAY = (Arg1 & 0x1F)
                        Local0 = ToDecimalString (GSER)
                        Local1 = ((Arg1 >> 0x05) & 0x0F)
                        Local2 = ((Arg1 >> 0x09) + 0x07BC)
                        Local3 = Concatenate (Local0, " ")
                        Local3 = Concatenate (Local3, ToDecimalString (GDAY))
                        Local3 = Concatenate (Local3, "/")
                        Local3 = Concatenate (Local3, ToDecimalString (Local1))
                        Local3 = Concatenate (Local3, "/")
                        Local3 = Concatenate (Local3, ToDecimalString (Local2))
                        Return (Local3)
                    }

                    Method (BTIF, 1, Serialized)
                    {
                        Local7 = (One << Arg0)
                        If ((BATP (One) == Zero))
                        {
                            Return (0xFF)
                        }

                        Acquire (ECMX, 0xFFFF)
                        Local0 = NGBF
                        Release (ECMX)
                        If (((Local0 & Local7) == Zero))
                        {
                            Return (Zero)
                        }

                        Acquire (ECMX, 0xFFFF)
                        If (ECAV ())
                        {
                            BSEL = Arg0
                            Local0 = BFCC
                            DerefOf (NBTI [Arg0]) [One] = BDCC
                            DerefOf (NBTI [Arg0]) [0x02] = Local0
                            DerefOf (NBTI [Arg0]) [0x04] = BDVO
                            Local4 = ((Local0 * NLB1) / 0x64)
                            DerefOf (NBTI [Arg0]) [0x05] = Local4
                            Local4 = ((Local0 * NLB2) / 0x64)
                            DerefOf (NBTI [Arg0]) [0x06] = Local4
                            DerefOf (NBTI [Arg0]) [0x0A] = GBSS (BSNO, BDAT)
                            Local1 = BCTL
                            DerefOf (NBTI [Arg0]) [0x09] = ToString (Mid (Local1, Zero, 0x08), Ones)
                        }

                        Release (ECMX)
                        Return (Zero)
                    }

                    Method (BTST, 2, Serialized)
                    {
                        Local7 = (One << Arg0)
                        If ((BATP (One) == Zero))
                        {
                            Return (0xFF)
                        }

                        Acquire (ECMX, 0xFFFF)
                        If (ECAV ())
                        {
                            BSEL = Arg0
                            Local0 = BSTS
                            Local3 = BCRN
                            DerefOf (NBST [Arg0]) [0x02] = BRCC
                            DerefOf (NBST [Arg0]) [0x03] = BPVO
                        }

                        Release (ECMX)
                        /* Bits 1 and 2 of the status: discharging, charging. */
                        Local1 = Zero
                        If ((Local0 & 0x02))
                        {
                            Local1 |= One
                        }

                        If ((Local0 & 0x04))
                        {
                            Local1 |= 0x02
                        }

                        If ((FindSetRightBit (Local1) == Zero))
                        {
                            Local3 = Zero
                        }
                        ElseIf ((Local3 >= 0x8000))
                        {
                            Local3 = (0x00010000 - Local3)
                        }

                        DerefOf (NBST [Arg0]) [Zero] = Local1
                        DerefOf (NBST [Arg0]) [One] = Local3
                        Return (Zero)
                    }

                    Device (BAT1)
                    {
                        Name (_HID, EisaId ("PNP0C0A"))
                        Name (_UID, One)
                        Name (_PCL, Package (0x01)
                        {
                            \_SB
                        })
                        Method (_STA, 0, NotSerialized)
                        {
                            If (BATP (One))
                            {
                                Return (0x1F)
                            }

                            Return (0x0F)
                        }

                        Method (_BIF, 0, NotSerialized)
                        {
                            If ((BTIF (One) == 0xFF))
                            {
                                Return (Package (0x0D)
                                {
                                    Zero, 0xFFFFFFFF, 0xFFFFFFFF, One, 0xFFFFFFFF, Zero, Zero, Zero, Zero, "", "", "", ""
                                })
                            }

                            Return (DerefOf (NBTI [One]))
                        }

                        Method (_BST, 0, NotSerialized)
                        {
                            Local0 = _UID
                            BTST (Local0, One)
                            Return (DerefOf (NBST [Local0]))
                        }
                    }
                }
            }

            /* BAT0: mAh from the EC, converted to mWh here. */
            Device (BAT0)
            {
                Name (_HID, EisaId ("PNP0C0A"))
                Name (_UID, Zero)
                Name (_PCL, Package (0x01)
                {
                    PCI0
                })
                Name (PUNT, One)
                Name (LFCC, 0x1770)
                Name (NBIF, Package (0x0D)
                {
                    Zero, Ones, Ones, One, Ones, Ones, Ones, Ones, Ones, "", "", "", ""
                })
                Name (PBIF, Package (0x0D)
                {
                    One, 0x1770, 0x1770, One, 0x39D0, 0x0258, 0x012C, 0x3C, 0x3C, "M3N", " ", "LIon", "OSyx"
                })
                Name (PBST, Package (0x04)
                {
                    Zero, Ones, Ones, Ones
                })
                Method (_STA, 0, NotSerialized)
                {
                    If (^^LPCB.EC0.BATP (Zero))
                    {
                        Return (0x1F)
                    }

                    Return (0x0F)
                }

                Method (FBIF, 5, NotSerialized)
                {
                    PUNT = Arg0
                    Local1 = Arg1
                    Local2 = Arg2
                    If ((PUNT == Zero))
                    {
                        Local1 *= 0x0A
                        Local2 *= 0x0A
                    }

                    PBIF [Zero] = Arg0
                    PBIF [One] = Local1
                    PBIF [0x02] = Local2
                    LFCC = Local2
                    PBIF [0x03] = Arg3
                    PBIF [0x04] = Arg4
                    Local5 = (Local1 / 0x0A)
                    PBIF [0x05] = Local5
                    Local6 = (Local1 / 0x14)
                    PBIF [0x06] = Local6
                }

                Method (CBIF, 0, NotSerialized)
                {
                    If (PUNT)
                    {
                        Local0 = DerefOf (PBIF [0x04])
                        PBIF [Zero] = Zero
                        PBIF [One] = ((DerefOf (PBIF [One]) * Local0) / 0x03E8)
                        PBIF [0x02] = ((DerefOf (PBIF [0x02]) * Local0) / 0x03E8)
                        PBIF [0x05] = ((DerefOf (PBIF [0x05]) * Local0) / 0x03E8)
                        PBIF [0x06] = ((DerefOf (PBIF [0x06]) * Local0) / 0x03E8)
                    }
                }

                Method (_BIF, 0, NotSerialized)
                {
                    If (!^^LPCB.EC0.BATP (Zero))
                    {
                        Return (NBIF)
                    }

                    Local0 = ^^LPCB.EC0.BIF0 ()
                    Local1 = ^^LPCB.EC0.BIF1 ()
                    Local2 = ^^LPCB.EC0.BIF2 ()
                    Local4 = ^^LPCB.EC0.BIF4 ()
                    If ((Local0 != Ones))
                    {
                        If ((Local1 != Ones))
                        {
                            If ((Local2 != Ones))
                            {
                                If ((Local4 != Ones))
                                {
                                    FBIF (Local0, Local1, Local2, One, Local4)
                                    CBIF ()
                                }
                            }
                        }
                    }

                    Return (PBIF)
                }

                Method (FBST, 4, NotSerialized)
                {
                    Local1 = (Arg1 & 0xFFFF)
                    Local0 = Zero
                    If (^^LPCB.EC0.ACAP ())
                    {
                        Local0 = One
                    }

                    If (Local0)
                    {
                        If ((Arg0 & One))
                        {
                            Local0 = 0x02
                        }
                        Else
                        {
                            Local0 = Zero
                        }
                    }
                    Else
                    {
                        Local0 = One
                    }

                    If ((Local1 >= 0x8000))
                    {
                        Local1 = (0xFFFF - Local1)
                    }

                    Local2 = Arg2
                    If ((PUNT == Zero))
                    {
                        Local1 *= 0x0A
                        Local2 *= 0x0A
                    }

                    Local3 = DerefOf (PBIF [0x04])
                    If (PUNT)
                    {
                        Local1 = ((Local1 * Local3) / 0x03E8)
                        Local2 = ((Local2 * Local3) / 0x03E8)
                    }

                    PBST [Zero] = Local0
                    PBST [One] = Local1
                    PBST [0x02] = Local2
                    PBST [0x03] = Arg3
                }

                Method (_BST, 0, NotSerialized)
                {
                    PBST [Zero] = Zero
                    PBST [One] = Ones
                    PBST [0x02] = Ones
                    PBST [0x03] = Ones
                    If (!^^LPCB.EC0.BATP (Zero))
                    {
                        Return (PBST)
                    }

                    Local0 = ^^LPCB.EC0.B0SS ()
                    Local1 = ^^LPCB.EC0.BCRT ()
                    Local2 = ^^LPCB.EC0.BRCP ()
                    Local3 = ^^LPCB.EC0.BVOL ()
                    If ((Local0 != Ones))
                    {
                        FBST (Local0, Local1, Local2, Local3)
                    }

                    Return (PBST)
                }
            }

            Device (AC0)
            {
                Name (_HID, "ACPI0003" /* Power Source Device */)
                Name (_PCL, Package (0x01)
                {
                    PCI0
                })
                Name (ACST, Package (0x04) { Zero, One, 0x02, 0x03 })
                Method (_PSR, 0, NotSerialized)
                {
                    /* Round trips that must not change the answer. */
                    Local0 = ^^LPCB.EC0.ACAP ()
                    Local1 = RefOf (Local0)
                    Local2 = Match (ACST, MEQ, DerefOf (Local1), MTR, Zero, Zero)
                    If ((Local2 == Ones))
                    {
                        Return (Zero)
                    }

                    Return (Local2)
                }
            }

            Device (LID0)
            {
                Name (_HID, EisaId ("PNP0C0D"))
                Method (_LID, 0, NotSerialized)
                {
                    Return ((BFLG & One))
                }
            }
        }
    }

    Scope (\_TZ)
    {
        ThermalZone (THM0)
        {
            Method (_TMP, 0, NotSerialized)
            {
                /* Tenths of a kelvin from a temperature in degrees Celsius. */
                Local0 = \_SB.PCI0.LPCB.EC0.TAH0
                Return (((Local0 * 0x0A) + 0x0AAC))
            }

            Method (_CRT, 0, NotSerialized)
            {
                Return (0x0E94)
            }
        }
    }
}
