/*
 * A battery, an AC adapter, a thermal zone and a lid for QEMU, which has
 * none: load it with
 *
 *   qemu-system-x86_64 ... -acpitable file=scripts/acpi/qemu-battery.aml
 *
 * and OSyx shows a battery in the task bar and in Settings > Power, read
 * by /bin/batteryd through the same firmware methods a laptop has (see
 * docs/testing.md, "Battery"). The battery starts at 76 % of a 50 Wh pack
 * that has aged to 45 Wh and discharges at 12 W, one percent every 9
 * seconds of the Timer operator, down to 5 %; then it charges back.
 *
 * Written for OSyx in the style laptop firmware uses (packages filled
 * through arguments, a mutex, strings in buffers). Compile with:
 *   iasl scripts/acpi/qemu-battery.asl
 */
DefinitionBlock ("", "SSDT", 2, "OSYX  ", "QEMUBAT ", 0x00000001)
{
    Scope (\_SB)
    {
        Mutex (BATM, 0x00)
        Name (BSEC, Zero)

        /* Seconds since the interpreter started, and the charge in mWh. */
        Method (SECS, 0, NotSerialized)
        {
            Return ((Timer / 0x00989680))
        }

        /* Arg0: package to fill (state, rate, remaining, voltage). */
        Method (GBST, 1, Serialized)
        {
            Acquire (BATM, 0xFFFF)
            Local0 = SECS ()
            /* A cycle: 71 steps down from 76 % to 5 %, then 71 back up. */
            Local1 = ((Local0 / 0x09) % 0x8E)
            If ((Local1 < 0x47))
            {
                Local2 = (0x4C - Local1)
                Arg0 [Zero] = One
                Arg0 [One] = 0x2EE0
            }
            Else
            {
                Local2 = (0x05 + (Local1 - 0x47))
                Arg0 [Zero] = 0x02
                Arg0 [One] = 0x4E20
            }

            Arg0 [0x02] = ((0xAFC8 * Local2) / 0x64)
            Arg0 [0x03] = (0x2710 + (Local2 * 0x14))
            BSEC = Local1
            Release (BATM)
            Return (Arg0)
        }

        Device (BAT0)
        {
            Name (_HID, EisaId ("PNP0C0A"))
            Name (_UID, Zero)
            Name (_PCL, Package (0x01)
            {
                \_SB
            })
            Name (BIXP, Package (0x15)
            {
                One, Zero, 0xC350, 0xAFC8, One, 0x2B5C, 0x1194, 0x08CA, 0x57, 0x00017318,
                0xFFFFFFFF, 0xFFFFFFFF, 0x03E8, 0x01F4, 0x0A, 0x0A, "", "", "", "", Zero
            })
            Name (BSTP, Package (0x04) {})
            Method (_STA, 0, NotSerialized)
            {
                Return (0x1F)
            }

            Method (_BIX, 0, NotSerialized)
            {
                Name (SERN, Buffer (0x06)
                {
                    "00000"
                })
                Local0 = 0x0BB9
                Local1 = 0x04
                While (Local0)
                {
                    Divide (Local0, 0x0A, Local2, Local0)
                    SERN [Local1] = (Local2 + 0x30)
                    Local1--
                }

                BIXP [0x10] = "OSYX-QEMU-50"
                BIXP [0x11] = SERN
                BIXP [0x12] = "LION"
                BIXP [0x13] = "OSyx"
                Return (BIXP)
            }

            Method (_BST, 0, NotSerialized)
            {
                Return (GBST (BSTP))
            }
        }

        Device (AC0)
        {
            Name (_HID, "ACPI0003" /* Power Source Device */)
            Name (_UID, Zero)
            Name (_PCL, Package (0x01)
            {
                \_SB
            })
            Method (_PSR, 0, NotSerialized)
            {
                /* Plugged in during the charging half of the cycle. */
                If ((((SECS () / 0x09) % 0x8E) < 0x47))
                {
                    Return (Zero)
                }

                Return (One)
            }
        }

        Device (LID0)
        {
            Name (_HID, EisaId ("PNP0C0D"))
            Method (_LID, 0, NotSerialized)
            {
                Return (One)
            }
        }
    }

    Scope (\_TZ)
    {
        ThermalZone (THM0)
        {
            Method (_TMP, 0, NotSerialized)
            {
                /* 45.0 to 49.9 degrees Celsius, in tenths of a kelvin. */
                Return ((0x0C6E + (\_SB.SECS () % 0x32)))
            }

            Method (_CRT, 0, NotSerialized)
            {
                Return (0x0E94)
            }
        }
    }
}
