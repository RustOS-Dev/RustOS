//! HD Audio codecs: the widget graph and the choice of playback and
//! capture paths through it.
//!
//! A codec's audio function group holds widgets (DACs, ADCs, mixers,
//! selectors, pin complexes) linked by connection lists. Pins carry a
//! default configuration from the BIOS saying what they are (line out,
//! speaker, headphone, microphone, line in) and whether they are wired at
//! all. Playback goes from a DAC to an output pin; capture from an input
//! pin to an ADC. The driver asks the codec for the widgets, builds a
//! [`Codec`], and programs the path this module returns.

use alloc::vec::Vec;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WidgetType {
    AudioOut,
    AudioIn,
    Mixer,
    Selector,
    Pin,
    Power,
    VolumeKnob,
    Beep,
    Other,
}

impl WidgetType {
    /// From the Audio Widget Capabilities parameter (bits 23:20).
    pub fn from_caps(caps: u32) -> WidgetType {
        match (caps >> 20) & 0xF {
            0 => WidgetType::AudioOut,
            1 => WidgetType::AudioIn,
            2 => WidgetType::Mixer,
            3 => WidgetType::Selector,
            4 => WidgetType::Pin,
            5 => WidgetType::Power,
            6 => WidgetType::VolumeKnob,
            7 => WidgetType::Beep,
            _ => WidgetType::Other,
        }
    }
}

/// Widget capability bits.
pub const WCAP_IN_AMP: u32 = 1 << 1;
pub const WCAP_OUT_AMP: u32 = 1 << 2;
pub const WCAP_AMP_OVERRIDE: u32 = 1 << 3;
pub const WCAP_CONN_LIST: u32 = 1 << 8;
pub const WCAP_POWER: u32 = 1 << 10;
pub const WCAP_DIGITAL: u32 = 1 << 9;
/// Pin capabilities.
pub const PINCAP_PRESENCE: u32 = 1 << 2;
pub const PINCAP_HP: u32 = 1 << 3;
pub const PINCAP_OUT: u32 = 1 << 4;
pub const PINCAP_IN: u32 = 1 << 5;
pub const PINCAP_HDMI: u32 = 1 << 7;
pub const PINCAP_EAPD: u32 = 1 << 16;
pub const PINCAP_DP: u32 = 1 << 24;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Widget {
    pub nid: u8,
    pub caps: u32,
    pub pin_caps: u32,
    /// Pin default configuration.
    pub config: u32,
    /// Nodes this widget takes input from.
    pub conns: Vec<u8>,
    pub amp_in: u32,
    pub amp_out: u32,
}

impl Widget {
    pub fn kind(&self) -> WidgetType {
        WidgetType::from_caps(self.caps)
    }
    pub fn digital(&self) -> bool {
        self.caps & WCAP_DIGITAL != 0
    }
}

/// What a pin's default configuration says it is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum PinKind {
    LineOut,
    Speaker,
    Headphone,
    Cd,
    SpdifOut,
    DigitalOut,
    ModemLine,
    ModemHandset,
    LineIn,
    Aux,
    Mic,
    Telephony,
    SpdifIn,
    DigitalIn,
    Other,
}

impl PinKind {
    pub fn from_config(cfg: u32) -> PinKind {
        match (cfg >> 20) & 0xF {
            0 => PinKind::LineOut,
            1 => PinKind::Speaker,
            2 => PinKind::Headphone,
            3 => PinKind::Cd,
            4 => PinKind::SpdifOut,
            5 => PinKind::DigitalOut,
            6 => PinKind::ModemLine,
            7 => PinKind::ModemHandset,
            8 => PinKind::LineIn,
            9 => PinKind::Aux,
            0xA => PinKind::Mic,
            0xB => PinKind::Telephony,
            0xC => PinKind::SpdifIn,
            0xD => PinKind::DigitalIn,
            _ => PinKind::Other,
        }
    }

    pub fn is_output(self) -> bool {
        matches!(
            self,
            PinKind::LineOut | PinKind::Speaker | PinKind::Headphone
        )
    }

    pub fn is_input(self) -> bool {
        matches!(self, PinKind::LineIn | PinKind::Mic | PinKind::Aux)
    }

    pub fn name(self) -> &'static str {
        match self {
            PinKind::LineOut => "line out",
            PinKind::Speaker => "speaker",
            PinKind::Headphone => "headphone",
            PinKind::LineIn => "line in",
            PinKind::Mic => "microphone",
            PinKind::Aux => "aux",
            PinKind::SpdifOut | PinKind::DigitalOut => "digital out",
            _ => "other",
        }
    }
}

/// Port connectivity (bits 31:30): 0 jack, 1 nothing, 2 fixed, 3 both.
pub fn pin_connected(cfg: u32) -> bool {
    (cfg >> 30) != 1
}

/// Whether the pin is a fixed (internal) device, e.g. a laptop speaker.
pub fn pin_fixed(cfg: u32) -> bool {
    (cfg >> 30) == 2
}

/// One hop of a path: a widget and, for widgets with several inputs, the
/// index of the connection to select (toward the converter).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Hop {
    pub nid: u8,
    pub select: Option<u8>,
}

/// A route between a pin and a converter: `hops[0]` is the pin, the last
/// hop the DAC or ADC.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Route {
    pub pin: u8,
    pub kind: PinKind,
    pub fixed: bool,
    pub hops: Vec<Hop>,
}

impl Route {
    pub fn converter(&self) -> u8 {
        self.hops.last().map_or(0, |h| h.nid)
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Codec {
    pub vendor: u32,
    pub afg: u8,
    pub widgets: Vec<Widget>,
}

impl Codec {
    pub fn widget(&self, nid: u8) -> Option<&Widget> {
        self.widgets.iter().find(|w| w.nid == nid)
    }

    /// Depth-first search from `from` toward a widget of type `target`
    /// through mixers and selectors (at most 5 hops).
    fn find(&self, from: u8, target: WidgetType, hops: &mut Vec<Hop>, depth: usize) -> bool {
        let Some(w) = self.widget(from) else {
            return false;
        };
        if depth > 0 && w.kind() == target {
            hops.push(Hop {
                nid: from,
                select: None,
            });
            return !w.digital();
        }
        if depth > 5 || (depth > 0 && !matches!(w.kind(), WidgetType::Mixer | WidgetType::Selector))
        {
            return false;
        }
        for (i, &c) in w.conns.iter().enumerate() {
            let n = hops.len();
            hops.push(Hop {
                nid: from,
                select: (w.conns.len() > 1).then_some(i as u8),
            });
            if self.find(c, target, hops, depth + 1) {
                return true;
            }
            hops.truncate(n);
        }
        false
    }

    /// Output routes: every connected output-capable pin with a path to a
    /// DAC, best first (headphone, line out, speaker).
    pub fn output_routes(&self) -> Vec<Route> {
        let mut v = Vec::new();
        for w in self.widgets.iter().filter(|w| w.kind() == WidgetType::Pin) {
            let kind = PinKind::from_config(w.config);
            if !pin_connected(w.config)
                || !kind.is_output()
                || w.pin_caps & PINCAP_OUT == 0
                || w.pin_caps & (PINCAP_HDMI | PINCAP_DP) != 0
            {
                continue;
            }
            let mut hops = Vec::new();
            if self.find(w.nid, WidgetType::AudioOut, &mut hops, 0) {
                v.push(Route {
                    pin: w.nid,
                    kind,
                    fixed: pin_fixed(w.config),
                    hops,
                });
            }
        }
        v.sort_by_key(|r| match r.kind {
            PinKind::Headphone => 0,
            PinKind::LineOut => 1,
            _ => 2,
        });
        v
    }

    /// Input routes: connected input pins with a path to an ADC (the ADC
    /// lists its sources, so the search runs from each ADC).
    pub fn input_routes(&self) -> Vec<Route> {
        let mut v = Vec::new();
        for adc in self
            .widgets
            .iter()
            .filter(|w| w.kind() == WidgetType::AudioIn && !w.digital())
        {
            for pin in self.widgets.iter().filter(|w| w.kind() == WidgetType::Pin) {
                let kind = PinKind::from_config(pin.config);
                if !pin_connected(pin.config) || !kind.is_input() || pin.pin_caps & PINCAP_IN == 0 {
                    continue;
                }
                let mut hops = Vec::new();
                if self.find_to(adc.nid, pin.nid, &mut hops, 0) {
                    // Store pin first, ADC last, like output routes.
                    hops.reverse();
                    v.push(Route {
                        pin: pin.nid,
                        kind,
                        fixed: pin_fixed(pin.config),
                        hops,
                    });
                }
            }
        }
        v.sort_by_key(|r| match r.kind {
            PinKind::Mic if !r.fixed => 0,
            PinKind::LineIn => 1,
            PinKind::Mic => 2,
            _ => 3,
        });
        v
    }

    /// Path from widget `from` back to `pin` (capture direction).
    fn find_to(&self, from: u8, pin: u8, hops: &mut Vec<Hop>, depth: usize) -> bool {
        let Some(w) = self.widget(from) else {
            return false;
        };
        if from == pin {
            hops.push(Hop {
                nid: from,
                select: None,
            });
            return true;
        }
        if depth > 5 || (depth > 0 && !matches!(w.kind(), WidgetType::Mixer | WidgetType::Selector))
        {
            return false;
        }
        for (i, &c) in w.conns.iter().enumerate() {
            let n = hops.len();
            hops.push(Hop {
                nid: from,
                select: (w.conns.len() > 1).then_some(i as u8),
            });
            if self.find_to(c, pin, hops, depth + 1) {
                return true;
            }
            hops.truncate(n);
        }
        false
    }

    /// The output to use: a plugged headphone, else the first route (a
    /// fixed speaker when nothing is plugged into a jack with presence
    /// detection).
    pub fn choose_output(&self, plugged: impl Fn(u8) -> bool) -> Option<Route> {
        let routes = self.output_routes();
        let detects = |r: &Route| {
            self.widget(r.pin)
                .is_some_and(|w| w.pin_caps & PINCAP_PRESENCE != 0)
        };
        if let Some(hp) = routes
            .iter()
            .find(|r| r.kind == PinKind::Headphone && (!detects(r) || plugged(r.pin)))
        {
            return Some(hp.clone());
        }
        routes
            .iter()
            .find(|r| r.kind != PinKind::Headphone)
            .or(routes.first())
            .cloned()
    }
}

/// Parse the text of a Linux codec dump (`/proc/asound/card0/codec#0`):
/// the vendor id and each node's capabilities, pin configuration and
/// connections. Used for tests with dumps from real machines.
pub fn parse_proc_codec(text: &str) -> Codec {
    let mut c = Codec::default();
    let hex = |s: &str| u32::from_str_radix(s.trim().trim_start_matches("0x"), 16).ok();
    let mut cur: Option<Widget> = None;
    let mut want_conns = false;
    for line in text.lines() {
        let t = line.trim();
        if let Some(v) = t.strip_prefix("Vendor Id:") {
            c.vendor = hex(v).unwrap_or(0);
        } else if let Some(r) = t.strip_prefix("Node ") {
            if let Some(w) = cur.take() {
                c.widgets.push(w);
            }
            want_conns = false;
            let nid = hex(r.split_whitespace().next().unwrap_or("")).unwrap_or(0) as u8;
            let caps = r
                .split("wcaps ")
                .nth(1)
                .and_then(|x| hex(x.split(':').next().unwrap_or("")))
                .unwrap_or(0);
            cur = Some(Widget {
                nid,
                caps,
                pin_caps: 0,
                config: 0x4000_0000,
                conns: Vec::new(),
                amp_in: 0,
                amp_out: 0,
            });
        } else if let Some(w) = cur.as_mut() {
            if want_conns {
                want_conns = false;
                w.conns = t
                    .split_whitespace()
                    .filter_map(|x| hex(x.trim_end_matches('*')).map(|v| v as u8))
                    .collect();
            } else if let Some(v) = t.strip_prefix("Pincap ") {
                w.pin_caps = hex(v.split(':').next().unwrap_or("")).unwrap_or(0);
            } else if let Some(v) = t.strip_prefix("Pin Default ") {
                w.config = hex(v.split(':').next().unwrap_or("")).unwrap_or(0);
            } else if t.starts_with("Connection:") {
                want_conns = true;
            }
        }
    }
    if let Some(w) = cur.take() {
        c.widgets.push(w);
    }
    c
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Nodes of a Realtek ALC269-style laptop codec (abridged dump):
    /// DACs 0x02/0x03, ADC 0x08 fed by mixer 0x23, output mixers
    /// 0x0c/0x0d, speaker 0x14 (fixed), headphone 0x21 (jack), internal
    /// mic 0x12, external mic 0x18.
    const ALC269: &str = "\
Codec: Realtek ALC269VB
Vendor Id: 0x10ec0269
Node 0x02 [Audio Output] wcaps 0x41d: Stereo Amp-Out
Node 0x03 [Audio Output] wcaps 0x41d: Stereo Amp-Out
Node 0x08 [Audio Input] wcaps 0x10011b: Stereo Amp-In
  Connection: 1
     0x23
Node 0x0c [Audio Mixer] wcaps 0x20010b: Stereo Amp-In
  Connection: 2
     0x02 0x0b
Node 0x0d [Audio Mixer] wcaps 0x20010b: Stereo Amp-In
  Connection: 2
     0x03 0x0b
Node 0x12 [Pin Complex] wcaps 0x40000b: Stereo Amp-In
  Pincap 0x00000020: IN
  Pin Default 0x90a60140: [Fixed] Mic at Int N/A
Node 0x14 [Pin Complex] wcaps 0x40018d: Stereo Amp-Out
  Pincap 0x00010014: OUT EAPD Detect
  Pin Default 0x90170110: [Fixed] Speaker at Int N/A
  Connection: 2
     0x0c* 0x0d
Node 0x18 [Pin Complex] wcaps 0x40018f: Stereo Amp-In Amp-Out
  Pincap 0x00003724: IN Detect
  Pin Default 0x03a11020: [Jack] Mic at Ext Left
  Connection: 2
     0x0c* 0x0d
Node 0x19 [Pin Complex] wcaps 0x40018f: Stereo Amp-In Amp-Out
  Pincap 0x00003724: IN Detect
  Pin Default 0x411111f0: [N/A] Speaker at Ext Rear
Node 0x21 [Pin Complex] wcaps 0x40018d: Stereo Amp-Out
  Pincap 0x0001001c: OUT HP EAPD Detect
  Pin Default 0x0321101f: [Jack] HP Out at Ext Left
  Connection: 2
     0x0c 0x0d*
Node 0x23 [Audio Mixer] wcaps 0x20010b: Stereo Amp-In
  Connection: 5
     0x18 0x19 0x1a 0x1b 0x12
";

    #[test]
    fn alc269_paths() {
        let c = parse_proc_codec(ALC269);
        assert_eq!(c.vendor, 0x10ec0269);
        assert_eq!(c.widgets.len(), 11);
        let outs = c.output_routes();
        assert_eq!(outs.len(), 2);
        assert_eq!((outs[0].kind, outs[0].pin), (PinKind::Headphone, 0x21));
        assert_eq!(outs[0].converter(), 0x02);
        assert_eq!(outs[0].hops[0].select, Some(0));
        // Nothing plugged in: the speaker.
        let o = c.choose_output(|_| false).unwrap();
        assert_eq!((o.pin, o.kind, o.fixed), (0x14, PinKind::Speaker, true));
        assert_eq!(
            o.hops.iter().map(|h| h.nid).collect::<Vec<_>>(),
            [0x14, 0x0c, 0x02]
        );
        // Headphones plugged: they win.
        assert_eq!(c.choose_output(|n| n == 0x21).unwrap().pin, 0x21);
        // Capture: the external mic first, through mixer 0x23 (input 0).
        let ins = c.input_routes();
        assert_eq!(ins[0].pin, 0x18);
        assert_eq!(ins[0].converter(), 0x08);
        assert_eq!(ins[0].hops.len(), 3);
        assert!(ins.iter().any(|r| r.pin == 0x12 && r.fixed));
    }

    #[test]
    fn qemu_duplex_codec() {
        // QEMU hda-duplex: DAC 0x02 -> line-out pin 0x03 (no connection
        // list entries beyond one), ADC 0x04 <- line-in pin 0x05.
        let c = Codec {
            vendor: 0x1af4_0022,
            afg: 1,
            widgets: alloc::vec![
                Widget {
                    nid: 2,
                    caps: 0x0000_0011,
                    pin_caps: 0,
                    config: 0,
                    conns: alloc::vec![],
                    amp_in: 0,
                    amp_out: 0
                },
                Widget {
                    nid: 3,
                    caps: 0x0040_0101,
                    pin_caps: PINCAP_OUT,
                    config: 0x0101_4010,
                    conns: alloc::vec![2],
                    amp_in: 0,
                    amp_out: 0
                },
                Widget {
                    nid: 4,
                    caps: 0x0010_0111,
                    pin_caps: 0,
                    config: 0,
                    conns: alloc::vec![5],
                    amp_in: 0,
                    amp_out: 0
                },
                Widget {
                    nid: 5,
                    caps: 0x0040_0001,
                    pin_caps: PINCAP_IN,
                    config: 0x0181_3020,
                    conns: alloc::vec![],
                    amp_in: 0,
                    amp_out: 0
                },
            ],
        };
        let o = c.choose_output(|_| false).unwrap();
        assert_eq!((o.pin, o.converter(), o.kind), (3, 2, PinKind::LineOut));
        let i = &c.input_routes()[0];
        assert_eq!((i.pin, i.converter(), i.kind), (5, 4, PinKind::LineIn));
    }
}
