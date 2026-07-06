//! Entry point: `sluss serve` runs the daemon (default), `sluss log`
//! reads the audit trail back out.

mod logcmd;
mod pipeline;
mod server;
mod verify;

use anyhow::{Result, bail};

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        None | Some("serve") => server::run(),
        Some("log") => logcmd::run(&args[1..]),
        Some(other) => {
            bail!("unknown command '{other}' — usage: sluss [serve | log [repo [number]]]")
        }
    }
}
