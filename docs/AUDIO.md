# Audio

RustOS plays and records sound through an OSS-compatible interface. The
sound core is in `src/sound/`, the drivers are in `src/sound/{hda,virtio_snd}.rs`
and `src/usb/audio.rs`, and the shared code, which is host-tested, is in
`crates/audio`.

## Devices

Each card gets `/dev/dspN` (PCM) and `/dev/mixerN` (volume). The first card
is also `/dev/dsp` and `/dev/mixer`. `dmesg | grep sound` lists the cards
in probe order: PCI cards first, then USB cards as they enumerate.

| Card | Driver | Playback | Capture |
|------|--------|----------|---------|
| Intel HD Audio (PCI class 04:03) | `hda` | yes | yes (first input path) |
| virtio-sound (QEMU `virtio-sound-pci`) | `virtio_snd` | yes | yes |
| USB Audio Class 1 and 2 | `snd-usb-audio` | yes | no |

Every card runs at 48 kHz, 16-bit stereo internally. A USB device that
lacks 48 kHz runs at 44.1, 32 or 16 kHz, in that order of preference, with
mono or 24/32-bit samples when it offers nothing better. Clients may open
`/dev/dsp` with any rate (8–192 kHz), 1–8 channels and U8, S16_LE, S24_LE
or S32_LE samples. The core converts to the card's format with a linear
resampler and a channel remix.

## The sound core

- **Mixer:**
  - Any number of processes can have `/dev/dsp` open for playback at the same time.
  - A per-card mixer thread sums the clients every 10 ms period, applies the master (`volume`) and `pcm` levels, and feeds the card.
  - Each client buffers up to 250 ms; writes block when the buffer is full, or return `EAGAIN` with `O_NONBLOCK`.
- **Idle:** the card stops about half a second after the last sample, and restarts on the next write.
- **OSS ioctls:** `SNDCTL_DSP_SPEED`, `SETFMT`, `CHANNELS`, `STEREO`, `GETFMTS`, `GETOSPACE`/`GETISPACE`, `GETODELAY`, `SYNC`, `RESET`, `SETFRAGMENT` (accepted, periods are fixed), and `SOUND_MIXER_READ/WRITE_VOLUME` and `_PCM`.
- **poll:** reports `POLLOUT` when a client has buffer room and `POLLIN` when recorded data is waiting.
- **Unplugging:** when a USB card is unplugged, its device nodes disappear and open files return `ENODEV`.

## Intel HD Audio

The controller driver sets up CORB/RIRB and discovers the codecs on the link. Then, for each codec:

- **Graph:** it reads the widget graph (AFG, pins with their default configuration, DACs, ADCs, mixers and selectors).
- **Output path:** `audio::hda::Codec::choose_output` picks the path. When a jack has presence detect and something is plugged in, headphones win. Otherwise the fixed speaker is used, then line out.
- **Input path:** the internal microphone, else the microphone jack, else line in.
- **Path setup:** every widget on the path is unmuted and set to 0 dB. The pins are enabled, EAPD is set, and GPIO 0 is raised, which switches the speaker amplifier on many laptops.
- **Jack changes:** the headphone jack is checked each time playback starts, not through unsolicited-response interrupts.
- **Streams:** one output and one input stream descriptor, each with 8 × 4 KiB buffer descriptors. Playback is paced by the link position (LPIB).
- **Volume:** the hardware amplifier follows the OSS master volume.

`crates/audio` tests path selection against `/proc/asound/card0/codec#0` dumps from real machines.

**Not supported:**
- HDMI and DisplayPort audio pins; ELD is not parsed.
- Codec-specific quirk tables, beyond EAPD and GPIO 0.
- Laptops whose speakers or microphones sit behind an Intel SOF DSP. Most recent Intel laptops wire their digital microphone arrays this way.

## USB Audio Class

`src/usb/audio.rs` binds to AudioStreaming interfaces. `usb_desc::uac` parses the class descriptors of both UAC1 and UAC2 (format type I, sample rates, feature units and clock sources). The driver then:
1. Picks an alternate setting with an isochronous OUT endpoint, preferring stereo 16-bit at 48 kHz.
2. Selects that alternate setting.
3. Sets the sampling frequency: through the endpoint for UAC1, or the first clock source for UAC2.
4. Unmutes the first feature unit at 0 dB. Volume is applied in software.

Playback is one isochronous TD per service interval (1 ms at full speed), with 16 in flight and scheduled "as soon as possible" (SIA). At 44.1 kHz the packet sizes alternate between 44 and 45 frames.

**Not supported:**
- Capture, and asynchronous feedback endpoints. Adaptive and synchronous devices, the common USB headsets and speakers, work.
- Devices that change the channel count or rate between alternate settings, beyond the choice above.

## virtio-sound

The driver sets up the stream parameters through the control queue. It sends 10 ms periods on the TX queue (4 in flight) and fills RX buffers for capture. It is the cheapest target for testing in QEMU.

## Tools

- `play [-d DEV] FILE...` plays WAV (8/16/24/32-bit PCM) and MP3 files. The MP3 decoder is `nanomp3`, a Rust port of minimp3, behind the `mp3` feature of `crates/audio`.
- `rec [-d DEV] [-t SECONDS] [-r RATE] [-c CH] FILE.wav` records 16-bit WAV.
- `beep [-f HZ] [-l MS] [-v PERCENT] [-d DEV]` plays a sine tone.
- `mixer [-d DEV] [volume|pcm [LEVEL]]` shows or sets the volumes.

## In the browser

`<audio>` and `new Audio(url)` play WAV and MP3 through `/dev/dsp` (see [JAVASCRIPT.md](JAVASCRIPT.md)):
- The browser fetches and decodes the clip, then plays it from a child process.
- **Supported:** `play()` (a promise), `pause()`, `currentTime` (seek), `duration`, `volume`, `muted`, `loop`, `ended`, `autoplay` and `<source>` children.
- **Events:** `loadedmetadata`, `canplay`, `play`, `playing`, `timeupdate`, `pause`, `ended` and `error`.
- **Not supported:** `<video>` (it plays nothing and reports an error) and Ogg/Vorbis.

## Testing

`tests/scenarios/audio` boots QEMU with four cards:
- `intel-hda` with `hda-duplex`, `virtio-sound-pci` and `usb-audio` on xHCI, each writing a WAV file through QEMU's `wav` audio backend;
- `ich9-intel-hda` with `hda-micro`, whose `none` backend can record.

The scenario plays a different tone on each output card, records a second of audio, plays an MP3 with `play`, and loads a page that plays an MP3 through `new Audio()`. `tools/check-wav.py` then checks each WAV on the host: it must contain enough loud audio, and the expected frequency must dominate (Goertzel power against neighbouring frequencies).
