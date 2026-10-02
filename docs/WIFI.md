# Wi-Fi

RustOS drives Intel Wi-Fi 6/6E adapters (AX210, and the AX211/AX201 CNVi
parts) in **station mode** and joins **open, WPA2-Personal (PSK) and
WPA3-Personal (SAE)** networks. The 802.11 MAC work (scanning, channel
switching, ACKs, retransmission, rate scaling, CCMP/GCMP encryption) runs in
the adapter's firmware; authentication, association and the WPA handshakes
run in the kernel.

Adapters with Linux drivers (MediaTek MT7921 PCIe cards and MT7921AU USB
adapters, M29/M30 onwards) use
Linux's 802.11 stack through [LinuxKPI](LINUXKPI.md), with **wpa_supplicant**
doing the security. `wifi` drives these adapters through wpa_supplicant's
control socket (`userland/nettools/src/wpa.rs`), starting wpa_supplicant when
needed. The commands are the same. Linux-driver interfaces also support:
- WPA2/WPA3-Enterprise:
  `wifi connect SSID PASSWORD --eap peap|ttls --identity ID [--ca CERT.pem]`;
- OWE;
- `hostapd` for access points.

The native AX210 driver stays the default for its cards until Linux iwlwifi
(M31) has passed `hwcheck` on hardware.

## Components

| Layer | Location | Role |
|-------|----------|------|
| 802.11 core + supplicant | `crates/wlan` (host-tested, `no_std`) | frames, IEs, RSN negotiation, WPA2 4-way/group handshakes, WPA3-SAE (hash-to-element and hunting-and-pecking), station state machine |
| Firmware files | `src/drivers/wifi/iwlwifi/fw.rs` | `.ucode` TLV parser (image loader, LMAC/UMAC/paging sections, capabilities, command versions), `.pnvm` SKU selection |
| PCIe transport | `iwlwifi/trans.rs` | context-info boot, RX ring, command queue, TX queues |
| Firmware API | `iwlwifi/mvm.rs` | command encoders (PHY/MAC/binding contexts, stations, keys, scan, TX, rate scaling) |
| Driver | `iwlwifi/mod.rs` | driver thread, scan/join/roam logic, `wlan0` `NetDevice`, ioctls |
| Tool | `userland/nettools/src/wifi.rs` | `wifi` command |

The `wlan` crate is tested on the host against IEEE 802.11 Annex J vectors
(PMK, PRF, PTK), RFC 4493 (AES-CMAC), RFC 3394 (key wrap) and SAE commit /
confirm / PMK vectors, plus full handshake simulations against a test
authenticator (`cd crates/wlan && cargo test`).

## Firmware

RustOS ships this firmware in every image (`/lib/firmware`, listed in
[firmware/stock.list](../firmware/stock.list) and fetched at build time from a
pinned linux-firmware release), so the adapter works without copying
anything. The files come from
[linux-firmware](https://git.kernel.org/pub/scm/linux/kernel/git/firmware/linux-firmware.git)
(`intel/iwlwifi/`):

| Adapter | PCI ID | Files |
|---------|--------|-------|
| AX210 (discrete) | 8086:2725 | `iwlwifi-ty-a0-gf-a0-72.ucode` (driver speaks the -72 API; -71/-68/-66/-63/-59 are tried as fallbacks) and `iwlwifi-ty-a0-gf-a0.pnvm` |
| AX211 (CNVi) | 8086:51F0/51F1/54F0/7A70/7AF0/7F70 | `iwlwifi-so-a0-gf-a0-72.ucode`, `iwlwifi-so-a0-gf-a0.pnvm` |
| AX201 (CNVi, HR RF) | same IDs, HR radio | `iwlwifi-so-a0-hr-b0-72.ucode` |

The kernel loads them from the first of `/lib/firmware`,
`/storage/lib/firmware`, `/boot/efi/firmware` and
`/boot/efi/EFI/rustos/firmware`. Firmware is retried every few seconds
after boot (the storage partition is mounted after drivers probe) and on
every `wifi` request, so copying the files in later works without a reboot.

`write_to_drive.sh` also copies them onto the storage partition, or a
different set when given one:

```sh
./write_to_drive.sh --drive /dev/sdX                          # stock firmware
./write_to_drive.sh --drive /dev/sdX --ax210-firmware ~/linux-firmware/intel/iwlwifi
RUSTOS_AX210_FIRMWARE=/path/to/iwlwifi-ty-a0-gf-a0-72.ucode ./write_to_drive.sh --drive /dev/sdX
```

Compressed distribution copies (`.xz`, `.zst`) are decompressed on the way.

## Using it

```
wifi                        # status of wlan0
wifi scan                   # BSSID, channel, signal, security, SSID
wifi connect "My Network" 'passphrase'
wifi connect "My Network" 'passphrase' --save   # also remember it
wifi connect CafeOpen       # open network
wifi disconnect
wifi forget "My Network"
wifi auto                   # join the first reachable saved network
wifi power                  # power-save state and why
wifi power on|off|auto      # auto: on while running on battery
```

Once associated, `wlan0` gets an address from the kernel's DHCP client
like any Ethernet interface (`ip addr`, `ping`, `wget` just work).
`/etc/rc` runs `wifi auto -q &` at boot. Saved networks live in
`/storage/etc/wifi.conf` (or `/etc/wifi.conf`):

```
ssid="My Network"
psk="passphrase"

ssid="CafeOpen"
```

Status is also visible in `/proc/net/wireless` and
`/sys/class/net/wlan0/wireless/status`.

### Tuning and fallbacks

If a network misbehaves with the fast modes, limit them in
`/storage/etc/kernel.conf` (read at boot) and reconnect:

```
iwlwifi.mode=vht     # legacy | ht | vht | he (default: he)
iwlwifi.width=40     # 20 | 40 | 80 | 160 MHz (default: 160)
iwlwifi.agg=0        # no A-MPDU aggregation
iwlwifi.rxbuf=4k     # 4 KiB receive buffers (advertise 3895-byte MPDUs)
iwlwifi.baid=sta     # set up receive block ack via ADD_STA
iwlwifi.power=off    # on | off | auto (default: auto)
iwlwifi.6ghz=0       # do not scan 6 GHz
iwlwifi.btcoex=0     # WiFi owns the shared antenna (no BT coexistence)
```

## How a connection works

1. **Boot**: the transport lays out the context-info structures (image
   loader, DRAM copies of the runtime sections, RX and command rings),
   kicks the firmware and waits for ALIVE; the PNVM section matching the
   reported SKU is handed over; `INIT_EXTENDED_CFG`, `NVM_ACCESS_COMPLETE`
   and `INIT_COMPLETE` finish the unified init. The driver then sends the
   antenna, SoC, Bluetooth-coexistence, PHY-context, power, regulatory
   (MCC, world domain) and scan configuration, and adds the station MAC
   context.
2. **Scan**: a UMAC scan (v15) over 2.4 GHz channels 1–13 and the 5 GHz
   UNII bands (the firmware keeps DFS/restricted channels passive);
   beacons and probe responses fill the BSS table with signal strength.
   When the regulatory domain allows 6 GHz (AX210/AX211), the scan adds
   the 15 preferred scanning channels (5, 21, ... 229), listening only,
   plus every 6 GHz channel that a 2.4/5 GHz AP announced in a Reduced
   Neighbor Report, probing for the reported BSSIDs and short SSIDs.
3. **Join**: the strongest BSS for the SSID (5 and 6 GHz slightly preferred) is
   chosen; the PHY context is moved to its channel, the MAC is bound to it,
   the AP station is added with management and data TX queues, and
   session protection keeps the radio on channel.
4. **Authenticate / associate** via `wlan::sta::Station`: Open System or
   SAE commit/confirm, then association with our RSN element (PMF
   negotiated; required for WPA3).
5. **Keys**: the 4-way handshake derives the PTK/GTK (and IGTK with PMF);
   keys go into the firmware (`ADD_STA_KEY`, `MGMT_MCAST_KEY`), which
   encrypts and decrypts data frames in hardware. The link parameters
   negotiated from the AP's HT/VHT/HE elements (`wlan::caps`) go into the
   PHY context (width, control channel position), MAC context (HT
   protection, EDCA from WMM, 11ax), ADD_STA (width, MIMO, A-MPDU limits),
   the HE station context and the rate-scaling configuration
   (`TLC_MNG_CONFIG`).
6. **Data**: Ethernet frames from the network stack are wrapped as
   802.11 QoS data frames on the TX queue of their access category (from
   the IP DSCP) with per-TID sequence numbers; a busy TID gets a block-ack
   session (ADDBA) and the firmware aggregates it. Received MPDUs are
   unwrapped after the firmware's decryption, reordered within block-ack
   sessions and checked against replay (CCMP/GCMP packet numbers).
7. **Roaming/loss**: missed-beacon notifications or a deauthentication
   tear the link down and trigger a new scan for the saved SSID.

## Status and limitations

The driver is written against the Linux iwlwifi v6.6 sources and the -72
firmware API, and verified against the real firmware files (TLV parsing,
PNVM selection, command sizes). It could not be exercised on hardware in
CI (no emulator exists), so expect rough edges on real laptops; the kernel
log (`dmesg`) prints each boot step and firmware error details.

* 802.11n/ac/ax (HT/VHT/HE) on 2.4 and 5 GHz: 20/40/80/160 MHz,
  2 spatial streams, LDPC, STBC, firmware rate scaling. The negotiated
  mode and the firmware's current TX rate show in `wifi status`
  (`mode "802.11ax 80 MHz 2x2"`, `rate "HE-MCS 11 2SS 80MHz"`).
* A-MPDU aggregation both ways (block-ack sessions with a reorder
  buffer driven by the firmware's release notifications); A-MSDUs are
  received (split by the hardware, or in software) but not sent.
  Receive buffers are 12 KiB, so the driver advertises 11454-byte MPDUs
  (VHT, HE 6 GHz) and 7935-byte A-MSDUs (HT). Receive block-ack sessions
  use `RX_BAID_ALLOCATION_CONFIG` on firmware that has it, and `ADD_STA`
  otherwise.
* 6 GHz (HE only): 20–160 MHz from the AP's 6 GHz Operation
  Information, the HE 6 GHz Band Capabilities element, and WPA3-SAE with
  hash-to-element only (WPA3 on 6 GHz requires H2E). `wifi scan` shows
  6 GHz channels as `37/6G`.
* Power save: while associated, the firmware sleeps between beacons
  (`MAC_PM_POWER_TABLE`, balanced scheme, low-power RX) and filters
  unchanged beacons. It wakes on traffic by itself, staying awake for
  100 ms after the last frame. In `auto` mode power save is on while an
  ACPI AC adapter (`_PSR`) reports that the machine runs on battery.
  uAPSD is not used.
* Bluetooth coexistence uses the firmware's shared-antenna arbitration
  (`BT_CONFIG` mode NW), so the card's Bluetooth core can run alongside.
* The channel list follows the regulatory domain the firmware reports.
* WPA-Enterprise (802.1X/EAP), WEP and TKIP-only networks are refused.
* No AP, monitor or P2P modes.
