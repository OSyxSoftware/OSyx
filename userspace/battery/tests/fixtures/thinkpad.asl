/*
 * A laptop firmware table written for the OSyx battery tests, in the style
 * of the ThinkPad "H8" embedded controller firmware: the battery data sits
 * behind a page register (HIID), the info and status packages are filled
 * through Index stores into packages passed as arguments, strings come from
 * 128-bit EC fields, and the methods create named objects while they run.
 *
 * Not copied from any vendor's table. Compile with:
 *   iasl -tc thinkpad.asl   (produces thinkpad.aml)
 */
DefinitionBlock ("", "DSDT", 2, "OSYX  ", "TPSTYLE ", 0x00000001)
{
    Name (OSYS, Zero)
    Name (H8DR, Zero)
    Name (BSWR, Zero)
    Name (BSWA, Zero)

    Scope (\_SB)
    {
        Method (_INI, 0, NotSerialized)
        {
            If (CondRefOf (\_OSI))
            {
                If (\_OSI ("Windows 2009")) { OSYS = 0x07D9 }
                If (\_OSI ("Windows 2012")) { OSYS = 0x07DC }
                If (\_OSI ("Windows 2015")) { OSYS = 0x07DF }
            }
        }

        Device (PCI0)
        {
            Name (_HID, EisaId ("PNP0A08"))
            Name (_CID, EisaId ("PNP0A03"))
            Name (_ADR, Zero)

            Device (LPC0)
            {
                Name (_ADR, 0x00140003)

                Device (EC0)
                {
                    Name (_HID, EisaId ("PNP0C09"))
                    Name (_UID, Zero)
                    Name (_GPE, 0x03)
                    Name (_CRS, ResourceTemplate ()
                    {
                        IO (Decode16, 0x0062, 0x0062, 0x01, 0x01, )
                        IO (Decode16, 0x0066, 0x0066, 0x01, 0x01, )
                    })
                    Method (_REG, 2, NotSerialized)
                    {
                        If ((Arg0 == 0x03))
                        {
                            H8DR = Arg1
                        }
                    }

                    OperationRegion (ECOR, EmbeddedControl, Zero, 0x0100)
                    Field (ECOR, ByteAcc, NoLock, Preserve)
                    {
                        HDBM,   1,
                            ,   1,
                            ,   1,
                        HFNE,   1,
                            ,   1,
                            ,   1,
                        HLDM,   1,
                        Offset (0x01),
                        BBLS,   1,
                        BTCM,   1,
                        Offset (0x38),
                        HB0S,   7,
                        HB0A,   1,
                        HB1S,   7,
                        HB1A,   1,
                        Offset (0x46),
                            ,   1,
                            ,   1,
                            ,   2,
                        HPAC,   1,
                        Offset (0x81),
                        HIID,   8
                    }

                    Field (ECOR, ByteAcc, NoLock, Preserve)
                    {
                        Offset (0xA0),
                        SBRC,   16,
                        SBFC,   16,
                        SBAE,   16,
                        SBRS,   16,
                        SBAC,   16,
                        SBVO,   16,
                        SBAF,   16,
                        SBBS,   16
                    }

                    Field (ECOR, ByteAcc, NoLock, Preserve)
                    {
                        Offset (0xA0),
                        SBBM,   16,
                        SBMD,   16,
                        SBCC,   16
                    }

                    Field (ECOR, ByteAcc, NoLock, Preserve)
                    {
                        Offset (0xA0),
                        SBDC,   16,
                        SBDV,   16,
                        SBOM,   16,
                        SBSI,   16,
                        SBDT,   16,
                        SBSN,   16
                    }

                    Field (ECOR, ByteAcc, NoLock, Preserve)
                    {
                        Offset (0xA0),
                        SBCH,   32
                    }

                    Field (ECOR, ByteAcc, NoLock, Preserve)
                    {
                        Offset (0xA0),
                        SBMN,   128
                    }

                    Field (ECOR, ByteAcc, NoLock, Preserve)
                    {
                        Offset (0xA0),
                        SBDN,   128
                    }

                    Mutex (BATM, 0x00)

                    Method (RBEC, 1, NotSerialized)
                    {
                        Return (Zero)
                    }

                    Method (GBIX, 3, Serialized)
                    {
                        Acquire (BATM, 0xFFFF)
                        If (Arg2)
                        {
                            HIID = (Arg0 | One)
                            Local7 = SBCC
                            Arg1 [0x08] = Local7
                            Local7 = SBBM
                            Local7 >>= 0x0F
                            Arg1 [One] = (Local7 ^ One)
                            HIID = Arg0
                            If (Local7)
                            {
                                Local1 = (SBFC * 0x0A)
                            }
                            Else
                            {
                                Local1 = SBFC
                            }

                            Arg1 [0x03] = Local1
                            HIID = (Arg0 | 0x02)
                            If (Local7)
                            {
                                Local0 = (SBDC * 0x0A)
                            }
                            Else
                            {
                                Local0 = SBDC
                            }

                            Arg1 [0x02] = Local0
                            Divide (Local1, 0x14, Local2, Arg1 [0x06])
                            If (Local7)
                            {
                                Arg1 [0x07] = 0xC8
                            }
                            ElseIf (SBDV)
                            {
                                Divide (0x00030D40, SBDV, Local2, Arg1 [0x07])
                            }
                            Else
                            {
                                Arg1 [0x07] = Zero
                            }

                            Arg1 [0x05] = SBDV
                            Local0 = SBSN
                            Name (SERN, Buffer (0x06)
                            {
                                "     "
                            })
                            Local2 = 0x04
                            While (Local0)
                            {
                                Divide (Local0, 0x0A, Local1, Local0)
                                SERN [Local2] = (Local1 + 0x30)
                                Local2--
                            }

                            Arg1 [0x11] = SERN
                            HIID = (Arg0 | 0x06)
                            Arg1 [0x10] = SBDN
                            HIID = (Arg0 | 0x04)
                            Name (BTYP, Buffer (0x05)
                            {
                                 0x00, 0x00, 0x00, 0x00, 0x00
                            })
                            BTYP = SBCH
                            Arg1 [0x12] = BTYP
                            HIID = (Arg0 | 0x05)
                            Arg1 [0x13] = SBMN
                        }
                        Else
                        {
                            Arg1 [0x02] = 0xFFFFFFFF
                            Arg1 [0x03] = 0xFFFFFFFF
                            Arg1 [0x05] = 0xFFFFFFFF
                            Arg1 [0x06] = Zero
                            Arg1 [0x07] = Zero
                        }

                        Release (BATM)
                        Return (Arg1)
                    }

                    Method (GBST, 4, NotSerialized)
                    {
                        Acquire (BATM, 0xFFFF)
                        If ((Arg1 & 0x20))
                        {
                            Local0 = 0x02
                        }
                        ElseIf ((Arg1 & 0x40))
                        {
                            Local0 = One
                        }
                        Else
                        {
                            Local0 = Zero
                        }

                        If ((Arg1 & 0x07)) {}
                        Else
                        {
                            Local0 |= 0x04
                        }

                        If (((Arg1 & 0x07) == 0x07))
                        {
                            Local0 = 0x04
                            Local1 = Zero
                            Local2 = Zero
                            Local3 = Zero
                        }
                        Else
                        {
                            HIID = Arg0
                            Local3 = SBVO
                            If (Arg2)
                            {
                                Local2 = (SBRC * 0x0A)
                            }
                            Else
                            {
                                Local2 = SBRC
                            }

                            Local1 = SBAC
                            If ((Local1 >= 0x8000))
                            {
                                If ((Local0 & One))
                                {
                                    Local1 = (0x00010000 - Local1)
                                }
                                Else
                                {
                                    Local1 = Zero
                                }
                            }
                            ElseIf (!(Local0 & 0x02))
                            {
                                Local1 = Zero
                            }

                            If (Arg2)
                            {
                                Local1 = ((Local3 * Local1) / 0x03E8)
                            }
                        }

                        Local5 = (One << (Arg0 >> 0x04))
                        BSWA |= BSWR
                        If (((BSWA & Local5) == Zero))
                        {
                            Arg3 [Zero] = Local0
                            Arg3 [One] = Local1
                            Arg3 [0x02] = Local2
                            Arg3 [0x03] = Local3
                        }

                        Release (BATM)
                        Return (Arg3)
                    }

                    Device (BAT0)
                    {
                        Name (_HID, EisaId ("PNP0C0A"))
                        Name (_UID, Zero)
                        Name (_PCL, Package (0x01)
                        {
                            \_SB
                        })
                        Name (B0ST, Zero)
                        Name (BT0I, Package (0x0D)
                        {
                            Zero, 0xFFFFFFFF, 0xFFFFFFFF, One, 0x2A30, Zero, Zero, One, One, "", "", "", ""
                        })
                        Name (BX0I, Package (0x15)
                        {
                            One, Zero, 0xFFFFFFFF, 0xFFFFFFFF, One, 0xFFFFFFFF, Zero, Zero, 0xFFFFFFFF, 0x00017318,
                            0xFFFFFFFF, 0xFFFFFFFF, 0x03E8, 0x01F4, 0xFFFFFFFF, 0xFFFFFFFF, "", "", "", "", Zero
                        })
                        Name (BT0P, Package (0x04) {})
                        Method (_STA, 0, NotSerialized)
                        {
                            If (\H8DR)
                            {
                                B0ST = HB0A
                            }
                            ElseIf ((RBEC (0x38) & 0x80))
                            {
                                B0ST = One
                            }
                            Else
                            {
                                B0ST = Zero
                            }

                            If (B0ST)
                            {
                                Return (0x1F)
                            }
                            Else
                            {
                                Return (0x0F)
                            }
                        }

                        Method (_BIX, 0, NotSerialized)
                        {
                            Local7 = Zero
                            Local6 = 0x0A
                            While ((!Local7 && Local6))
                            {
                                If (HB0A)
                                {
                                    If (((HB0S & 0x07) == 0x07))
                                    {
                                        Sleep (0x03E8)
                                        Local6--
                                    }
                                    Else
                                    {
                                        Local7 = One
                                    }
                                }
                                Else
                                {
                                    Local6 = Zero
                                }
                            }

                            Return (GBIX (Zero, BX0I, Local7))
                        }

                        Method (_BIF, 0, NotSerialized)
                        {
                            Local0 = _BIX ()
                            BT0I [Zero] = DerefOf (BX0I [One])
                            BT0I [One] = DerefOf (BX0I [0x02])
                            BT0I [0x02] = DerefOf (BX0I [0x03])
                            BT0I [0x03] = DerefOf (BX0I [0x04])
                            BT0I [0x04] = DerefOf (BX0I [0x05])
                            BT0I [0x05] = DerefOf (BX0I [0x06])
                            BT0I [0x06] = DerefOf (BX0I [0x07])
                            BT0I [0x07] = DerefOf (BX0I [0x0E])
                            BT0I [0x08] = DerefOf (BX0I [0x0F])
                            BT0I [0x09] = DerefOf (BX0I [0x10])
                            BT0I [0x0A] = DerefOf (BX0I [0x11])
                            BT0I [0x0B] = DerefOf (BX0I [0x12])
                            BT0I [0x0C] = DerefOf (BX0I [0x13])
                            Return (BT0I)
                        }

                        Method (_BST, 0, NotSerialized)
                        {
                            Local0 = (DerefOf (BX0I [One]) ^ One)
                            Return (GBST (Zero, HB0S, Local0, BT0P))
                        }

                        Method (_BTP, 1, NotSerialized)
                        {
                            HFNE = One
                        }
                    }

                    Device (AC)
                    {
                        Name (_HID, "ACPI0003")
                        Name (_UID, Zero)
                        Name (_PCL, Package (0x01)
                        {
                            \_SB
                        })
                        Method (_PSR, 0, NotSerialized)
                        {
                            If (\H8DR)
                            {
                                Return (HPAC)
                            }
                            ElseIf ((RBEC (0x46) & 0x10))
                            {
                                Return (One)
                            }
                            Else
                            {
                                Return (Zero)
                            }
                        }

                        Method (_STA, 0, NotSerialized)
                        {
                            Return (0x0F)
                        }
                    }
                }
            }
        }
    }
}
