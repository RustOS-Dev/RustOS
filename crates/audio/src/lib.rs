//! Audio building blocks shared by the kernel sound drivers, the userland
//! tools and host tests: PCM sample formats and conversion, a resampler,
//! a mixer, WAV files, a tone generator, and the HD Audio codec model that
//! picks the playback and capture paths through a codec's widgets.

#![no_std]

extern crate alloc;

pub mod hda;
pub mod pcm;
pub mod wav;
