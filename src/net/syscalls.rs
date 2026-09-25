//! Socket syscalls.

use crate::errno::*;

pub fn dispatch(_n: u64, _a: [u64; 6]) -> KResult<i64> {
    Err(EAFNOSUPPORT)
}
