use copy_confirmer::*;
use std::ffi::OsString;
use std::cmp::max;

use clap::Parser;
use colored::Colorize;

#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Args {
    /// Name of the person to greet
    #[arg(long, short, required(true))]
    source: OsString,

    /// Destination directories
    #[arg(long, short, required(true))]
    destination: Vec<OsString>,

    /// Number of threads for checksum calculation
    #[arg(long, short, default_value_t = 1)]
    jobs: usize,
}

fn main() -> Result<(), ConfirmerError> {
    let args = Args::parse();

    let num_threads = max(1, args.jobs);

    let cc = CopyConfirmer::new(num_threads);

    match cc.compare(args.source, args.destination)? {
        ConfirmerResult::Ok => {
            println!("All files present in destinations");
        }
        ConfirmerResult::MissingFiles(files) => {
            println!("{}", "Missing files:".red().bold());
            for file in files {
                println!("{file:?}");
            }
        }
    }
    Ok(())
}
