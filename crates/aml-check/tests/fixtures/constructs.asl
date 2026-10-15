/*
 * AML constructs the RustOS interpreter had to learn for real firmware
 * (an AMD Ryzen AI 300 laptop): index and bank fields, DerefOf of buffer
 * elements, references and field units, buffer fields as operands.
 * Compile with `iasl constructs.asl` (constructs.aml is committed).
 */
DefinitionBlock ("", "DSDT", 2, "RUSTOS", "AMLTEST", 1)
{
    /* An index/data port pair (as at 0x72/0x73 on AMD): FRTB is bytes 8..11. */
    OperationRegion (CMIO, SystemIO, 0x72, 0x02)
    Field (CMIO, ByteAcc, NoLock, Preserve)
    {
        CIDX,   8,
        CDAT,   8
    }
    IndexField (CIDX, CDAT, ByteAcc, NoLock, Preserve)
    {
        Offset (0x08),
        FRTB,   32
    }
    /* A region whose base address is read through the index field at load time. */
    OperationRegion (FRTP, SystemMemory, FRTB, 0x0100)
    Name (IXRD, Zero)
    IXRD = FRTB
    Method (IXWR, 1, NotSerialized)
    {
        FRTB = Arg0
        Return (FRTB)
    }

    /* A bank field: BSEL selects bank 1, then the data port is written. */
    OperationRegion (BSIO, SystemIO, 0x80, 0x01)
    Field (BSIO, ByteAcc, NoLock, Preserve)
    {
        BSEL,   8
    }
    OperationRegion (BKIO, SystemIO, 0x90, 0x01)
    BankField (BKIO, BSEL, 0x01, ByteAcc, NoLock, Preserve)
    {
        BNK1,   8
    }
    Method (BKWR, 1, NotSerialized)
    {
        BNK1 = Arg0
    }

    Name (ADAT, Buffer (0x04) { 0x11, 0x22, 0x33, 0x44 })
    /* DerefOf (Index (buffer, n)): the byte. */
    Method (DIDX, 1, NotSerialized)
    {
        Return (DerefOf (ADAT [Arg0]))
    }
    /* DerefOf (reference): the object referred to. */
    Method (DREF, 0, NotSerialized)
    {
        Local0 = RefOf (IXRD)
        Return (DerefOf (Local0))
    }
    /* DerefOf of a reference to a field unit: the field's value. */
    Method (DFLD, 0, NotSerialized)
    {
        If (CondRefOf (CDAT, Local0))
        {
            Return (DerefOf (Local0))
        }
        Return (Ones)
    }

    Name (CRS1, Buffer (0x06) { 0x00, 0x00, 0x00, 0x00, 0x00, 0x00 })
    /* A buffer field stored into a local is its value; the local decrements. */
    Method (BFDC, 0, Serialized)
    {
        CreateWordField (CRS1, 0x02, LEN0)
        LEN0 = 0x10
        Local0 = LEN0
        Local0--
        Return (Local0)
    }
    /* Increment of a buffer field writes it back. */
    Method (BFIN, 0, Serialized)
    {
        CreateWordField (CRS1, 0x04, CNT0)
        CNT0 = 0x05
        CNT0++
        Return (CNT0)
    }
    /* Buffer fields in comparisons and conversions. */
    Method (BFEQ, 0, Serialized)
    {
        CreateWordField (CRS1, 0x02, LEN1)
        If ((LEN1 == 0x10))
        {
            Return (ToBuffer (LEN1))
        }
        Return (Zero)
    }
}
