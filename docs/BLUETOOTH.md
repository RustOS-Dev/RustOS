# Bluetooth

RustOS connects Bluetooth keyboards and mice over both Bluetooth Low Energy (HID over GATT) and classic Bluetooth (BR/EDR HID). Pairing is done with the `bt` command. Paired devices reconnect automatically.

The stack has three parts:
- `crates/bt`, host-tested, holds the protocol pieces: HCI, L2CAP, ATT/GATT, SMP, SDP, HOGP/HIDP and the Intel firmware format.
- `src/bluetooth/` runs controllers and connections.
- The transports are `src/usb/btusb.rs` for USB and `src/bluetooth/h4.rs` for UART.

## Controllers

| Transport | Devices | Notes |
|-----------|---------|-------|
| USB (class E0/01/01) | Intel AX200/AX210/AX211 Bluetooth (8087:0029/0032/0033), most USB dongles | Commands on the control pipe, events on interrupt IN, ACL on bulk. SCO (voice) is not used. |
| H4 UART | any H4 controller on a legacy serial port | `bt attach com2`, or `bt.h4=com2` in `/storage/etc/kernel.conf`. COM2 is interrupt-driven; COM3 and COM4 are polled. |

**Intel controllers** start in a bootloader. The driver:
1. Reads the TLV version.
2. Loads `intel/ibt-XXXX-YYYY.sfi`, named from the CNVi/CNVR ids (`ibt-0041-0041` on AX210), from the firmware directories. `write_to_drive.sh --ax210-firmware DIR` installs it together with the Wi-Fi firmware when DIR is a linux-firmware tree.
3. Sends the image with Intel secure-send: the CSS header, key and signature, then the command payload in 4-byte-aligned groups.
4. Boots it with Intel Reset at the address from the image.
5. Applies `intel/ibt-*.ddc`.

The controller's firmware must be present. Without it, `dmesg` names the missing file.

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

- **Host tests:** `crates/bt` checks the SMP crypto against the Core specification's sample data (AES-CMAC, f4/f5/f6/g2, ah, c1/s1, the debug P-256 key pair). It also runs the pairing state machine against an independent responder in every association model (Just Works, numeric comparison, passkey both ways, legacy), and covers the HCI/L2CAP/ATT/SDP codecs, GATT discovery with long reads, HOGP against an in-memory server, and the Intel firmware fragmenting.
- **`tests/scenarios/bluetooth`:**
  - Runs an H4 controller on COM2, connected to `tools/fake-hci.py`: a scripted LE controller with a BLE HID keyboard, whose own SMP, AES and P-256 code is separate Python.
  - The scenario scans, pairs with LE Secure Connections (the fake checks the LTK the host encrypts with), gets key events on the keyboard's evdev node, and checks the bond file.
  - It then disconnects, waits for the automatic reconnection with the stored key, and removes the device.

## Not supported

- **Audio:** A2DP and SCO (headsets), so no Bluetooth audio.
- **Roles and services:** peripheral and advertising roles, a GATT server (requests to ours get "not found"), LE audio, mesh, file transfer and networking profiles.
- **Legacy PIN pairing** for pre-2.1 BR/EDR devices.
- **Unverified parts:** BR/EDR HID and the USB transport run only on real hardware and are untested in QEMU. The Intel firmware download follows Linux btintel and has not yet run on an AX210.
