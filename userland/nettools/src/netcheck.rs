//! netcheck: is there Internet access, or a captive portal in the way?

use rustos_rt::prelude::*;
use webclient::portal::{self, Status};

/// Exit status: 0 online, 1 no connectivity, 2 captive portal.
pub fn netcheck(args: &[String]) -> i32 {
    let quiet = args.iter().any(|a| a == "-q");
    let timeout = args
        .iter()
        .position(|a| a == "-T")
        .and_then(|i| args.get(i + 1))
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(8);
    if args.iter().any(|a| a == "-h" || a == "--help") {
        println!("usage: netcheck [-q] [-T SECS]");
        println!(
            "Probes {} (set url= and expect= in /etc/portal.conf).",
            portal::DEFAULT_PROBE
        );
        return 0;
    }
    if let Some((ifc, src, url)) = portal::announced()
        && !quiet
    {
        println!(
            "{}: network announces a captive portal: {} ({})",
            ifc, url, src
        );
    }
    let st = portal::check(timeout * 1000);
    if !quiet || st != Status::Online {
        println!("{}", portal::describe(&st));
    }
    match st {
        Status::Online => 0,
        Status::Offline(_) => 1,
        Status::Portal(_) => 2,
    }
}
