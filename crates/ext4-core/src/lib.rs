//! Pure ext4 logic shared by the kernel driver and host tests: checksums
//! (crc32c metadata checksums, crc16 group descriptors), directory index
//! hashes, and the jbd2 journal format (replay planning and building
//! transactions).

#![no_std]

extern crate alloc;

pub mod casefold;
#[rustfmt::skip]
mod casefold_data;
pub mod csum;
pub mod hash;
pub mod jbd2;
