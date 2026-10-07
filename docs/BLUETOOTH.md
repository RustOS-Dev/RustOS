# Bluetooth

RustOS connects Bluetooth keyboards and mice over both Bluetooth Low Energy (HID over GATT) and classic Bluetooth (BR/EDR HID). Pairing is done with the `bt` command. Paired devices reconnect automatically.

The stack has three parts:
- `crates/bt`, host-tested, holds the protocol pieces: HCI, L2CAP, ATT/GATT, SMP, SDP, HOGP/HIDP and the Intel firmware format.
- `src/bluetooth/` runs controllers and connections.
- The transports are `src/usb/btusb.rs` for USB and `src/bluetooth/h4.rs` for UART.

## Controllers

| Transport | Devices | Notes |
|-----------|---------|-------|
| USB (class E0/01/01) | Intel AX200/AX210/AX211 Bluetooth (8087:0029/0032/0033), MediaTek MT7921/MT7922/MT7925 (laptop combo cards and the MT7921AU adapter), Realtek RTL8723/8761/8821/8822/8851/8852/8922 (combo cards and dongles), Broadcom BCM20702/4335/4350 family, most USB dongles | Commands on the control pipe, events on interrupt IN, ACL on bulk. SCO (voice) is not used. |
| H4 UART | any H4 controller on a legacy serial port | `bt attach com2`, or `bt.h4=com2` in `/storage/etc/kernel.conf`. COM2 is interrupt-driven; COM3 and COM4 are polled. |

**Intel controllers** start in a bootloader. The driver:
1. Reads the TLV version.
2. Loads `intel/ibt-XXXX-YYYY.sfi`, named from the CNVi/CNVR ids (`ibt-0041-0041` on AX210), from the firmware directories. `write_to_drive.sh --ax210-firmware DIR` installs it together with the Wi-Fi firmware when DIR is a linux-firmware tree.
3. Sends the image with Intel secure-send: the CSS header, key and signature, then the command payload in 4-byte-aligned groups.
4. Boots it with Intel Reset at the address from the image.
5. Applies `intel/ibt-*.ddc`.

**Realtek controllers** (Bluetooth manufacturer 93 in Read Local Version; as Linux btrtl):
1. Identifies the chip from the LMP subversion, HCI revision and version (Linux's table of USB parts). If it already runs firmware it is told to drop it (0xFC66) and identified again.
2. Reads the ROM version (0xFC6D) and security key id, and loads `rtl_bt/<chip>_fw.bin` (RTL8852C: `_fw_v2.bin` first) and its `_config.bin` when there is one.
3. Picks the patch for that ROM version: one entry of an "epatch" v1 file, or the matching subsections of a v2 ("RTBTCore") file in priority order. Appends the config, then sends it with 0xFC20 in 252-byte fragments.

**Broadcom controllers** (manufacturer 15; as Linux btbcm): after a reset the driver reads the USB ids (0xFC5A). If `brcm/<chip>-<vid>-<pid>.hcd` or `brcm/BCM-<vid>-<pid>.hcd` is installed, it replays the file's HCI records after "download minidriver" (0xFC2E) and resets. Otherwise the controller runs its ROM firmware. linux-firmware has no Broadcom `.hcd` files, so none ship in the image.

**MediaTek controllers** (MT7961 = MT7921, MT7922, MT7925, and MT7902/MT6639 as Linux btmtk handles them). The driver:
1. Reads the chip ID, firmware version and flavour from chip registers (vendor control requests).
2. Loads `mediatek/BT_RAM_CODE_MT*_hdr.bin` (`mediatek/mt7925/` for MT7925).
3. Downloads each section of the patch with WMT commands. Each section is announced, then sent in 250-byte blocks over HCI vendor command 0xFC6F. The answers are polled with a vendor control-IN request.
4. Resets the endpoint options and turns the Bluetooth function on (WMT function control).

The patch format is in `crates/bt/src/mtk.rs`, which is host-tested against the linux-firmware files.

The controller's firmware must be present. It ships in the image (`firmware/stock.list`). Without it, `dmesg` names the missing file.

## Using it

```
bt status                  controller, address, version, connections
bt scan [SECONDS]          LE scan and BR/EDR inquiry (default 5 s)
bt devices                 what the last scans found
bt pair ADDR               connect, pair, bond, and set up HID
bt connect ADDR            reconnect a paired device now
bt disconnect ADDR
bt remove ADDR             forget a device
bt list                    paired devices
bt power on|off
bt attach PORT             start an H4 controller (com2, com3, com4)
```

**Pairing prompts.** A pairing may ask something:
- Keyboards pairing over LE or BR/EDR show "Type 123456 on the device, then Enter".
- A device with a display asks "Does the device show 123456? [yes/no]".
- A device that displays a passkey asks for it.
Answer on the terminal running `bt pair`.

**After pairing**, the device gets its own `/dev/input/eventN` (see `evtest -l`). Keyboards type into the console like USB keyboards, and their lock LEDs follow the console.

## Security

- **LE pairing** uses LE Secure Connections (P-256 ECDH with the f4/f5/f6/g2 functions), falling back to legacy pairing only for devices without it.
  - We offer KeyboardDisplay IO capabilities and ask for MITM protection. Devices without input or output pair with Just Works.
  - Keys are distributed both ways: the peer's IRK and identity address, and our IRK.
  - Resolvable private addresses of bonded devices are resolved with their IRK.
- **BR/EDR** uses Secure Simple Pairing, run by the controller; we answer its IO capability, confirmation and passkey requests. Legacy PIN pairing is refused.
- **Bonds** live in `/storage/etc/bluetooth/keys` (or `/etc/bluetooth/keys` without a storage partition), one line per device. They hold the LTK, EDIV/Rand and IRK, or the link key.

## Reconnection

- **LE:** while bonded LE devices are disconnected, the controller scans passively. When a bonded device advertises (by address, or by an RPA that resolves to its IRK), the kernel connects, encrypts with the stored LTK, and restarts HID.
- **BR/EDR:** page scan is on, so bonded BR/EDR devices connect to us. We accept, answer the link-key request, and wait for their HID control and interrupt channels.

## HID

- **LE (HID over GATT):**
  - Discovery reads the device name, the PnP ID (vendor and product for evdev), the battery level and the HID service's report map.
  - It enables notifications on the input reports, taking their report IDs from the Report Reference descriptors, and selects report protocol.
  - Output reports carry keyboard LEDs.
- **BR/EDR (HIDP):**
  - The HID descriptor comes from the device's SDP record (attribute 0x0206).
  - The interrupt channel carries input reports.
  - If SDP gives no descriptor, the device is put in boot protocol and read with the standard boot keyboard and mouse layouts.
- **Both** feed the same report parser and evdev code as USB HID (`usb::hid::HidSink`), including typematic repeat for keyboards, which only report changes.

## Testing

- **Host tests:** `crates/bt` checks the SMP crypto against the Core specification's sample data (AES-CMAC, f4/f5/f6/g2, ah, c1/s1, the debug P-256 key pair). It also runs the pairing state machine against an independent responder in every association model (Just Works, numeric comparison, passkey both ways, legacy), and covers the HCI/L2CAP/ATT/SDP codecs, GATT discovery with long reads, HOGP against an in-memory server, the Intel firmware fragmenting, the Realtek patch selection (v1 and v2 files; checked against every `rtl_bt` file of linux-firmware 20260916 during development) and fragment indices, the Broadcom `.hcd` records, and the MediaTek patch sections (on the linux-firmware files when they have been fetched).
- **`tests/scenarios/bluetooth`:**
  - Runs an H4 controller on COM2, connected to `tools/fake-hci.py`: a scripted LE controller with a BLE HID keyboard, whose own SMP, AES and P-256 code is separate Python.
  - The scenario scans, pairs with LE Secure Connections (the fake checks the LTK the host encrypts with), gets key events on the keyboard's evdev node, and checks the bond file.
  - It then disconnects, waits for the automatic reconnection with the stored key, and removes the device.

## Not supported

- **Audio:** A2DP and SCO (headsets), so no Bluetooth audio.
- **Roles and services:** peripheral and advertising roles, a GATT server (requests to ours get "not found"), LE audio, mesh, file transfer and networking profiles.
- **Legacy PIN pairing** for pre-2.1 BR/EDR devices.
- **Unverified parts:** BR/EDR HID and the USB transport run only on real hardware and are untested in QEMU. The Intel firmware download follows Linux btintel and has not yet run on an AX210. The MediaTek download follows Linux btmtk and has not yet run on a MediaTek controller. The Realtek and Broadcom downloads follow btrtl and btbcm and have not run on hardware either.
