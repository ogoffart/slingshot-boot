/*
 * A laptop battery and AC adapter for QEMU (scripts/run-qemu.sh --battery),
 * to try the System screen's battery card. Every 6 readings (30 s on the
 * System screen) it switches between charging and discharging.
 *
 * There is an embedded controller too, without hardware behind it: reading
 * it times out, as on a machine whose EC doesn't answer.
 */
DefinitionBlock ("", "SSDT", 2, "SLING", "BATTERY", 1)
{
    Scope (\_SB)
    {
        Device (EC0)
        {
            Name (_HID, EisaId ("PNP0C09"))
            Name (_CRS, ResourceTemplate () {
                IO (Decode16, 0x62, 0x62, 0, 1)
                IO (Decode16, 0x66, 0x66, 0, 1)
            })
            Name (ECOK, 0)
            OperationRegion (ERAM, EmbeddedControl, 0, 0xFF)
            Field (ERAM, ByteAcc, NoLock, Preserve)
            {
                Offset (0x10),
                TEMP, 8,
            }
            Method (_REG, 2)
            {
                If (Arg0 == 3)
                {
                    ECOK = Arg1
                }
            }
        }

        // Readings so far, and the charge in tenths of a percent.
        Name (CNT, 0)
        Name (LVL, 520)

        Device (AC)
        {
            Name (_HID, "ACPI0003")
            Method (_PSR)
            {
                Return (((CNT / 6) & 1) == 0)
            }
        }

        Device (BAT0)
        {
            Name (_HID, EisaId ("PNP0C0A"))
            Name (_UID, 1)
            Method (_STA) { Return (0x1F) }
            Name (_BIX, Package () {
                1,              // revision
                0,              // mWh
                57000,          // design capacity
                50160,          // last full charge
                1,              // rechargeable
                11520,          // design voltage, mV
                5700, 1140,
                312,            // cycle count
                95000,
                0xFFFFFFFF, 0xFFFFFFFF, 0xFFFFFFFF, 0xFFFFFFFF,
                100, 100,
                "5B10W13930",   // model
                "4721",         // serial number
                "LiP",          // type
                "SLINGSHOT",    // OEM
                1
            })
            Name (BSTP, Package () { 0, 0, 0, 12100 })
            Method (_BST)
            {
                If (\_SB.EC0.ECOK)
                {
                    BSTP [3] = 12000 + \_SB.EC0.TEMP
                }
                If (\_SB.AC._PSR ())
                {
                    LVL = LVL + 25
                    If (LVL > 1000) { LVL = 1000 }
                    BSTP [0] = 2
                    BSTP [1] = 24500
                }
                Else
                {
                    LVL = LVL - 15
                    BSTP [0] = 1
                    BSTP [1] = 9800
                }
                BSTP [2] = LVL * 50160 / 1000
                CNT++
                Return (BSTP)
            }
        }
    }
}
