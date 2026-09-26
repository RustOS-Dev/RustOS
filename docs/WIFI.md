# Wi-Fi

RustOS drives Intel Wi-Fi 6/6E adapters (AX210, and the AX211/AX201 CNVi
parts) in **station mode** and joins **open, WPA2-Personal (PSK) and
WPA3-Personal (SAE)** networks. The 802.11 MAC work (scanning, channel
switching, ACKs, retransmission, rate scaling, CCMP/GCMP encryption) runs in
the adapter's firmware; authentication, association and the WPA handshakes
run in the kernel.

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

The adapter needs Intel's firmware from
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

`write_to_drive.sh` provisions them onto the storage partition:

```sh
./write_to_drive.sh --drive /dev/sdX                          # auto-detect /lib/firmware
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
3. **Join**: the strongest BSS for the SSID (5 GHz slightly preferred) is
   chosen; the PHY context is moved to its channel, the MAC is bound to it,
   the AP station is added with management and data TX queues, and
   session protection keeps the radio on channel.
4. **Authenticate / associate** via `wlan::sta::Station`: Open System or
   SAE commit/confirm, then association with our RSN element (PMF
   negotiated; required for WPA3).
5. **Keys**: the 4-way handshake derives the PTK/GTK (and IGTK with PMF);
   keys go into the firmware (`ADD_STA_KEY`, `MGMT_MCAST_KEY`), which
   encrypts and decrypts data frames in hardware. Rate scaling is handed
   to the firmware (`TLC_MNG_CONFIG`).
6. **Data**: Ethernet frames from the network stack are wrapped as
   802.11 (QoS) data frames; received MPDUs are unwrapped after the
   firmware's decryption (IV, MIC and padding removed).
7. **Roaming/loss**: missed-beacon notifications or a deauthentication
   tear the link down and trigger a new scan for the saved SSID.

## Status and limitations

The driver is written against the Linux iwlwifi v6.6 sources and the -72
firmware API, and verified against the real firmware files (TLV parsing,
PNVM selection, command sizes). It could not be exercised on hardware in
CI (no emulator exists), so expect rough edges on real laptops; the kernel
log (`dmesg`) prints each boot step and firmware error details.

* Legacy rates only (HT/VHT/HE capabilities are not advertised yet):
  up to 54 Mb/s.
* No 6 GHz scanning, no A-MPDU/A-MSDU aggregation, no power save.
* WPA-Enterprise (802.1X/EAP), WEP and TKIP-only networks are refused.
* No AP, monitor or P2P modes.
