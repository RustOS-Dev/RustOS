#![no_std]
#![no_main]

use rustos_rt::prelude::*;

rustos_rt::entry!(main);

fn main(args: Vec<String>) -> i32 {
    eprintln!("{}: networking is not available yet", args[0]);
    1
}
