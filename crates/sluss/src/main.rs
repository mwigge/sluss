//! Entry point: `sluss serve` runs the daemon (default), `sluss log`
//! reads the audit trail back out.

mod logcmd;
mod pipeline;
mod server;
mod verify;

use std::path::PathBuf;

use anyhow::{Context, Result, bail};

/// Where the audit store lives: `SLUSS_DB` if set, otherwise
/// `$XDG_DATA_HOME/sluss/sluss.db` (`~/.local/share/sluss/sluss.db`).
/// A fixed default so `sluss log` reads the same db the daemon writes,
/// no matter which directory either was started from.
pub fn db_path() -> Result<PathBuf> {
    if let Ok(path) = std::env::var("SLUSS_DB") {
        return Ok(PathBuf::from(path));
    }
    let data_home = match std::env::var("XDG_DATA_HOME") {
        Ok(dir) if !dir.is_empty() => PathBuf::from(dir),
        _ => PathBuf::from(std::env::var("HOME").context("neither SLUSS_DB, XDG_DATA_HOME nor HOME is set")?)
            .join(".local/share"),
    };
    let dir = data_home.join("sluss");
    std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
    Ok(dir.join("sluss.db"))
}

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
