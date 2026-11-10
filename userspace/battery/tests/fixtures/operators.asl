/*
 * Operator checks for the AML interpreter OSyx runs firmware with, written
 * for the OSyx battery tests and compiled by the reference compiler, so the
 * encodings are the ones real firmware contains. Every method Tnnn returns
 * One when the interpreter computed what the ACPI specification says, and
 * the number of the first failed check otherwise.
 * Compile with: iasl -oa operators.asl (no constant folding: the
 * interpreter does the arithmetic, not the compiler)
 */
DefinitionBlock ("", "DSDT", 2, "OSYX  ", "OPERATOR", 0x00000001)
{
    Name (GINT, 0x1234)
    Name (GSTR, "battery")
    Name (GBUF, Buffer (0x08) { 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08 })
    Name (GPKG, Package (0x04) { 0x0A, 0x14, 0x1E, 0x28 })
    Name (GNST, Package (0x02)
    {
        Package (0x02) { One, 0x02 },
        Package (0x02) { 0x03, 0x04 }
    })
    Name (GEMP, Package (0x03) {})
    Mutex (GMTX, 0x00)
    Event (GEVT)

    /* Integer arithmetic and bit operations. */
    Method (T001, 0, NotSerialized)
    {
        If (((0x07 + 0x05) != 0x0C)) { Return (0x02) }
        If (((0x07 - 0x05) != 0x02)) { Return (0x03) }
        If (((0x07 * 0x05) != 0x23)) { Return (0x04) }
        If (((0x17 / 0x05) != 0x04)) { Return (0x05) }
        If (((0x17 % 0x05) != 0x03)) { Return (0x06) }
        Divide (0x17, 0x05, Local0, Local1)
        If (((Local0 != 0x03) || (Local1 != 0x04))) { Return (0x07) }
        If (((0xF0 & 0x3C) != 0x30)) { Return (0x08) }
        If (((0xF0 | 0x0F) != 0xFF)) { Return (0x09) }
        If (((0xF0 ^ 0xFF) != 0x0F)) { Return (0x0A) }
        If ((~0xFFFFFFFFFFFFFFF0 != 0x0F)) { Return (0x0B) }
        If (((One << 0x0C) != 0x1000)) { Return (0x0C) }
        If (((0x1000 >> 0x04) != 0x0100)) { Return (0x0D) }
        If ((NAnd (0xFF, 0x0F) != 0xFFFFFFFFFFFFFFF0)) { Return (0x0E) }
        If ((NOr (0xF0, 0x0F) != 0xFFFFFFFFFFFFFF00)) { Return (0x0F) }
        Local2 = 0x09
        Local2++
        Local2++
        Local2--
        If ((Local2 != 0x0A)) { Return (0x10) }
        Local2 += 0x05
        Local2 <<= One
        If ((Local2 != 0x1E)) { Return (0x11) }
        If ((FindSetLeftBit (0x90) != 0x08)) { Return (0x12) }
        If ((FindSetRightBit (0x90) != 0x05)) { Return (0x13) }
        If ((FindSetRightBit (Zero) != Zero)) { Return (0x14) }
        If ((FromBCD (0x1234) != 0x04D2)) { Return (0x15) }
        If ((ToBCD (0x04D2) != 0x1234)) { Return (0x16) }
        If (((0x0A - 0x0B) != Ones)) { Return (0x17) }
        Return (One)
    }

    /* Logic and comparisons. */
    Method (T002, 0, NotSerialized)
    {
        If ((0x05 > 0x06)) { Return (0x02) }
        If ((0x06 < 0x05)) { Return (0x03) }
        If ((0x05 >= 0x06)) { Return (0x04) }
        If ((0x06 <= 0x05)) { Return (0x05) }
        If (!(0x05 <= 0x05)) { Return (0x06) }
        If (!(0x05 >= 0x05)) { Return (0x07) }
        If ((One && Zero)) { Return (0x08) }
        If (!(One || Zero)) { Return (0x09) }
        If (("abc" != "abc")) { Return (0x0A) }
        If (("abc" >= "abd")) { Return (0x0B) }
        If ((GSTR != "battery")) { Return (0x0C) }
        If ((GINT != 0x1234)) { Return (0x0D) }
        Local0 = Buffer (0x03) { 0x01, 0x02, 0x03 }
        Local1 = Buffer (0x03) { 0x01, 0x02, 0x03 }
        If ((Local0 != Local1)) { Return (0x0E) }
        Local2 = Zero
        If ((Local2 == Zero)) { Local2 = 0x05 } ElseIf ((Local2 == 0x05)) { Local2 = 0x06 } Else { Local2 = 0x07 }
        If ((Local2 != 0x05)) { Return (0x0F) }
        Return (One)
    }

    /* Loops, Break, Continue, Switch. */
    Method (SWCH, 1, Serialized)
    {
        Switch (ToInteger (Arg0))
        {
            Case (One) { Return (0x0B) }
            Case (Package (0x02) { 0x02, 0x03 }) { Return (0x16) }
            Case (0x04) { Local0 = 0x21 }
            Default { Local0 = 0x2C }
        }

        Return (Local0)
    }

    Method (T003, 0, NotSerialized)
    {
        Local0 = Zero
        Local1 = Zero
        While ((Local0 < 0x0A))
        {
            Local0++
            If ((Local0 == 0x03)) { Continue }
            If ((Local0 == 0x08)) { Break }
            Local1 += Local0
        }

        /* 1 + 2 + 4 + 5 + 6 + 7 */
        If ((Local1 != 0x19)) { Return (0x02) }
        If ((SWCH (One) != 0x0B)) { Return (0x03) }
        If ((SWCH (0x03) != 0x16)) { Return (0x04) }
        If ((SWCH (0x04) != 0x21)) { Return (0x05) }
        If ((SWCH (0x09) != 0x2C)) { Return (0x06) }
        Return (One)
    }

    /* Strings and buffers. */
    Method (T004, 0, NotSerialized)
    {
        If ((ToInteger ("0x1A") != 0x1A)) { Return (0x02) }
        If ((ToInteger ("123") != 0x7B)) { Return (0x03) }
        If ((ToDecimalString (0x7B) != "123")) { Return (0x04) }
        Local0 = Concatenate ("AB", "CD")
        If ((Local0 != "ABCD")) { Return (0x05) }
        If ((SizeOf (Local0) != 0x04)) { Return (0x06) }
        If ((Mid ("battery", 0x02, 0x03) != "tte")) { Return (0x07) }
        Local1 = Buffer (0x05) { "BAT1" }
        If ((ToString (Local1, Ones) != "BAT1")) { Return (0x08) }
        If ((ToString (Local1, 0x02) != "BA")) { Return (0x09) }
        If ((SizeOf (Local1) != 0x05)) { Return (0x0A) }
        If ((DerefOf (Local1 [0x02]) != 0x54)) { Return (0x0B) }
        Local1 [0x03] = 0x32
        If ((ToString (Local1, Ones) != "BAT2")) { Return (0x0C) }
        If ((DerefOf (GSTR [One]) != 0x61)) { Return (0x0D) }
        Local2 = ToBuffer (0x0102)
        If ((SizeOf (Local2) != 0x08)) { Return (0x0E) }
        If ((DerefOf (Local2 [One]) != One)) { Return (0x0F) }
        Local3 = Concatenate (Buffer (0x02) { 0x01, 0x02 }, Buffer (0x01) { 0x03 })
        If ((SizeOf (Local3) != 0x03)) { Return (0x10) }
        If ((DerefOf (Local3 [0x02]) != 0x03)) { Return (0x11) }
        Local4 = Mid (GBUF, 0x02, 0x03)
        If ((DerefOf (Local4 [Zero]) != 0x03)) { Return (0x12) }
        If ((SizeOf (Local4) != 0x03)) { Return (0x13) }
        If ((SizeOf (GSTR) != 0x07)) { Return (0x14) }
        /* An integer made from a buffer: little endian. */
        If ((ToInteger (Buffer (0x02) { 0x34, 0x12 }) != 0x1234)) { Return (0x15) }
        Return (One)
    }

    /* Buffer fields. */
    Method (T005, 0, NotSerialized)
    {
        Local0 = Buffer (0x10) {}
        CreateByteField (Local0, Zero, BF08)
        CreateWordField (Local0, One, BF16)
        CreateDWordField (Local0, 0x04, BF32)
        CreateQWordField (Local0, 0x08, BF64)
        CreateBitField (Local0, 0x1F, BF01)
        CreateField (Local0, 0x18, 0x04, BF04)
        BF08 = 0xAB
        BF16 = 0x1234
        BF32 = 0xDEADBEEF
        BF64 = 0x0102030405060708
        BF01 = One
        BF04 = 0x05
        If ((BF08 != 0xAB)) { Return (0x02) }
        If ((BF16 != 0x1234)) { Return (0x03) }
        If ((BF32 != 0xDEADBEEF)) { Return (0x04) }
        If ((BF64 != 0x0102030405060708)) { Return (0x05) }
        If ((DerefOf (Local0 [One]) != 0x34)) { Return (0x06) }
        If ((DerefOf (Local0 [0x03]) != 0x85)) { Return (0x07) }
        If ((DerefOf (Local0 [0x0F]) != One)) { Return (0x08) }
        If ((BF04 != 0x05)) { Return (0x09) }
        /* Fields of a named buffer keep following it after a store to it. */
        CreateWordField (GBUF, 0x02, GW16)
        If ((GW16 != 0x0403)) { Return (0x0A) }
        GBUF = Buffer (0x08) { 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88 }
        If ((GW16 != 0x4433)) { Return (0x0B) }
        GBUF = Buffer (0x08) { 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08 }
        Return (One)
    }

    /* Packages, by value where the specification copies. */
    Method (T006, 0, NotSerialized)
    {
        If ((SizeOf (GPKG) != 0x04)) { Return (0x02) }
        If ((DerefOf (GPKG [0x02]) != 0x1E)) { Return (0x03) }
        Local0 = GPKG
        Local0 [0x02] = 0x63
        If ((DerefOf (GPKG [0x02]) != 0x1E)) { Return (0x04) }
        If ((DerefOf (Local0 [0x02]) != 0x63)) { Return (0x05) }
        If ((DerefOf (DerefOf (GNST [One]) [Zero]) != 0x03)) { Return (0x06) }
        DerefOf (GNST [One]) [Zero] = 0x09
        If ((DerefOf (DerefOf (GNST [One]) [Zero]) != 0x09)) { Return (0x07) }
        DerefOf (GNST [One]) [Zero] = 0x03
        If ((SizeOf (GEMP) != 0x03)) { Return (0x08) }
        GEMP [One] = "x"
        If ((DerefOf (GEMP [One]) != "x")) { Return (0x09) }
        Local1 = Package (0x03) { One, "two", Buffer (0x01) { 0x03 } }
        If ((ObjectType (DerefOf (Local1 [Zero])) != One)) { Return (0x0A) }
        If ((ObjectType (DerefOf (Local1 [One])) != 0x02)) { Return (0x0B) }
        If ((ObjectType (DerefOf (Local1 [0x02])) != 0x03)) { Return (0x0C) }
        If ((ObjectType (Local1) != 0x04)) { Return (0x0D) }
        Local2 = 0x05
        Local3 = Package (Local2) { One, 0x02 }
        If ((SizeOf (Local3) != 0x05)) { Return (0x0E) }
        If ((Match (GPKG, MEQ, 0x1E, MTR, Zero, Zero) != 0x02)) { Return (0x0F) }
        If ((Match (GPKG, MGT, 0x14, MLT, 0x28, Zero) != 0x02)) { Return (0x10) }
        If ((Match (GPKG, MEQ, 0x63, MTR, Zero, Zero) != Ones)) { Return (0x11) }
        Return (One)
    }

    /* Method calls: a package argument is the caller's package. */
    Method (FILL, 2, NotSerialized)
    {
        Arg0 [Zero] = Arg1
        Arg0 [One] = (Arg1 + One)
        Return (Arg0)
    }

    Method (FIL2, 2, NotSerialized)
    {
        Return (FILL (Arg0, Arg1))
    }

    Method (SUM7, 7, NotSerialized)
    {
        Return ((Arg0 + (Arg1 + (Arg2 + (Arg3 + (Arg4 + (Arg5 + Arg6)))))))
    }

    Method (FACT, 1, NotSerialized)
    {
        If ((Arg0 <= One)) { Return (One) }
        Return ((Arg0 * FACT ((Arg0 - One))))
    }

    Method (NONE, 0, NotSerialized)
    {
        Local0 = 0x05
    }

    Method (BUFW, 1, NotSerialized)
    {
        CreateByteField (Arg0, One, BYT1)
        BYT1 = 0x7E
    }

    Method (REBD, 1, NotSerialized)
    {
        /* A new object in the argument does not reach the caller. */
        Arg0 = Package (0x02) { 0x63, 0x63 }
        Arg0 [Zero] = 0x64
    }

    Method (T007, 0, NotSerialized)
    {
        Name (LPKG, Package (0x02) { Zero, Zero })
        Local0 = FILL (GEMP, 0x07)
        If ((DerefOf (Local0 [One]) != 0x08)) { Return (0x02) }
        If ((DerefOf (GEMP [Zero]) != 0x07)) { Return (0x03) }
        FIL2 (GEMP, 0x10)
        If ((DerefOf (GEMP [One]) != 0x11)) { Return (0x04) }
        FILL (LPKG, 0x20)
        If ((DerefOf (LPKG [Zero]) != 0x20)) { Return (0x05) }
        Local1 = Package (0x02) { Zero, Zero }
        FIL2 (Local1, 0x30)
        If ((DerefOf (Local1 [One]) != 0x31)) { Return (0x06) }
        If ((SUM7 (One, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07) != 0x1C)) { Return (0x07) }
        If ((FACT (0x06) != 0x02D0)) { Return (0x08) }
        Local2 = Buffer (0x02) { 0x01, 0x02 }
        BUFW (Local2)
        If ((DerefOf (Local2 [One]) != 0x7E)) { Return (0x09) }
        REBD (GEMP)
        If ((DerefOf (GEMP [Zero]) != 0x10)) { Return (0x0A) }
        NONE ()
        Return (One)
    }

    /* References, object existence, synchronisation objects, timing. */
    Method (T008, 0, Serialized)
    {
        Local0 = 0x55
        Local1 = RefOf (Local0)
        If ((DerefOf (Local1) != 0x55)) { Return (0x02) }
        If (!CondRefOf (GINT)) { Return (0x03) }
        If (CondRefOf (\NOPE)) { Return (0x04) }
        If (!CondRefOf (GINT, Local2)) { Return (0x05) }
        If ((DerefOf (Local2) != 0x1234)) { Return (0x06) }
        If (CondRefOf (\_SB.NOPE.DEEP, Local3)) { Return (0x07) }
        If (Acquire (GMTX, 0x03E8)) { Return (0x08) }
        Release (GMTX)
        Signal (GEVT)
        If (Wait (GEVT, 0x10)) { Return (0x09) }
        Reset (GEVT)
        Sleep (0x02)
        Stall (0x32)
        Notify (\_SB, 0x80)
        Local4 = Timer
        Debug = "trace"
        CopyObject ("text", Local5)
        If ((Local5 != "text")) { Return (0x0A) }
        If ((ObjectType (GMTX) != 0x09)) { Return (0x0B) }
        If ((ObjectType (T001) != 0x08)) { Return (0x0C) }
        Return (One)
    }

    /* Implicit conversions on a store to a named object. */
    Method (T009, 0, NotSerialized)
    {
        Name (NINT, Zero)
        Name (NSTR, "")
        Name (NBUF, Buffer (0x04) {})
        NINT = "0x20"
        If ((NINT != 0x20)) { Return (0x02) }
        NINT = Buffer (0x02) { 0x34, 0x12 }
        If ((NINT != 0x1234)) { Return (0x03) }
        NBUF = 0x0A0B0C0D
        If ((DerefOf (NBUF [Zero]) != 0x0D)) { Return (0x04) }
        If ((DerefOf (NBUF [0x03]) != 0x0A)) { Return (0x05) }
        If ((SizeOf (NBUF) != 0x04)) { Return (0x06) }
        NSTR = "abc"
        If ((NSTR != "abc")) { Return (0x07) }
        If ((ObjectType (NINT) != One)) { Return (0x08) }
        If ((ObjectType (NBUF) != 0x03)) { Return (0x09) }
        Return (One)
    }
}
