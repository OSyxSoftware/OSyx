/*
 * A secondary table (SSDT) written for the OSyx battery tests: the kinds
 * of objects firmware defines at table level, around a second AC adapter
 * it adds to the embedded controller of laptop.asl. Loading it must lose
 * nothing. Compile with: iasl ssdt.asl
 */
DefinitionBlock ("", "SSDT", 2, "OSYX  ", "EXTRAS  ", 0x00000001)
{
    External (\_SB.PCI0.LPCB.EC0, DeviceObj)
    External (\_SB.PCI0.LPCB.EC0.ACAP, MethodObj)
    External (\_SB.PCI0.NOPE, DeviceObj)
    External (OSYS, IntObj)

    Name (SSDT, Package (0x06)
    {
        "CPU0IST ", 0x80000000, 0x00000100,
        "CPU0CST ", 0x80000000, 0x00000200
    })
    Name (CFGD, 0x00000001)
    Name (BUF0, Buffer (0x10) {})
    CreateDWordField (BUF0, Zero, BF32)
    CreateField (BUF0, 0x20, 0x0C, BF12)
    Event (EVT0)
    Mutex (MTX0, 0x02)
    Alias (CFGD, CFG2)

    OperationRegion (GNVS, SystemMemory, 0xFE0C0000, 0x0100)
    Field (GNVS, AnyAcc, Lock, Preserve)
    {
        SMIF,   8,
        Offset (0x10),
        BNKS,   8,
        BNKD,   8
    }

    BankField (GNVS, BNKS, 0x02, ByteAcc, NoLock, Preserve)
    {
        Offset (0x20),
        BK2A,   8
    }

    DataTableRegion (DTR0, "FACP", "", "")
    Field (DTR0, AnyAcc, NoLock, Preserve)
    {
        Offset (0x24),
        FADT,   32
    }

    Scope (\_PR)
    {
        Processor (CPU0, 0x00, 0x00000410, 0x06) {}
        Processor (CPU1, 0x01, 0x00000410, 0x06)
        {
            Name (_PPC, Zero)
        }
    }

    PowerResource (PFAN, 0x00, 0x0000)
    {
        Method (_STA, 0, NotSerialized) { Return (One) }
        Method (_ON, 0, NotSerialized) {}
        Method (_OFF, 0, NotSerialized) {}
    }

    If (CondRefOf (\_SB.PCI0.NOPE))
    {
        Scope (\_SB.PCI0.NOPE)
        {
            Name (GONE, One)
        }
    }

    If (CondRefOf (\_SB.PCI0.LPCB.EC0))
    {
        Scope (\_SB.PCI0.LPCB.EC0)
        {
            OperationRegion (SMB0, GenericSerialBus, Zero, 0x0100)
            Field (SMB0, BufferAcc, NoLock, Preserve)
            {
                Connection (I2cSerialBusV2 (0x000B, ControllerInitiated, 0x000186A0, AddressingMode7Bit, "\\_SB.PCI0.I2C0", 0x00, ResourceConsumer, , Exclusive, )),
                AccessAs (BufferAcc, AttribBytes (0x02)),
                SBW0,   8
            }

            Device (ADP1)
            {
                Name (_HID, "ACPI0003" /* Power Source Device */)
                Name (_UID, One)
                Name (_PCL, Package (0x01)
                {
                    \_SB
                })
                Method (_PSR, 0, Serialized, 2)
                {
                    If ((CFG2 && (OSYS >= 0x07D0)))
                    {
                        Return (ACAP ())
                    }

                    Return (Zero)
                }
            }
        }
    }
    Else
    {
        Name (NOEC, One)
    }
}
