use std::path::PathBuf;
use std::process::ExitCode;

use opencode_storage::DatabaseLocation;
use opencode_storage::database::open;
use opencode_storage::dump::{Masks, dump};
use serde_json::json;

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("{message}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), String> {
    let mut args = std::env::args().skip(1);
    let command = args.next().ok_or_else(|| format!("missing <command>\n{}", usage()))?;
    let file = PathBuf::from(args.next().ok_or_else(|| format!("missing <file>\n{}", usage()))?);
    match command.as_str() {
        "open" => open_command(file, args),
        "dump" => {
            if args.next().is_some() {
                return Err(format!("dump accepts only <file>\n{}", usage()));
            }
            dump_command(file)
        }
        other => Err(format!("unknown command `{other}`\n{}", usage())),
    }
}

fn open_command(file: PathBuf, mut args: impl Iterator<Item = String>) -> Result<(), String> {
    let data = match (args.next(), args.next()) {
        (Some(flag), Some(dir)) if flag == "--data" => PathBuf::from(dir),
        _ => return Err(format!("open requires --data <dir>\n{}", usage())),
    };
    if args.next().is_some() {
        return Err(format!("open accepts only --data <dir> after <file>\n{}", usage()));
    }
    let (_database, report) = open(DatabaseLocation::File(file), &data).map_err(|error| error.to_string())?;
    println!(
        "{}",
        json!({
            "bootstrapped": report.bootstrapped,
            "applied": report.applied,
            "warnings": report.warnings,
        })
    );
    Ok(())
}

fn dump_command(file: PathBuf) -> Result<(), String> {
    let connection = rusqlite::Connection::open(file).map_err(|error| error.to_string())?;
    let dumped = dump(&connection, &Masks::new()).map_err(|error| error.to_string())?;
    println!("{dumped}");
    Ok(())
}

fn usage() -> String {
    "usage: storage open <file> --data <dir> | storage dump <file>".to_string()
}
