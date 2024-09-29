use copy_confirmer::*;
use std::cmp::max;
use std::ffi::OsString;
use std::fs::File;
use std::io::prelude::*;
use std::path::PathBuf;

use clap::Parser;
use colored::Colorize;
use serde::Serialize;

#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Args {
    /// Source directory
    #[arg(long, short, required(true))]
    source: OsString,

    /// Destination directories
    #[arg(long, short, required(true))]
    destination: Vec<OsString>,

    /// Number of threads for checksum calculation
    #[arg(long, short, default_value_t = 1)]
    jobs: usize,

    /// Print json output to this file
    #[arg(long, short)]
    out_file: Option<OsString>,

    /// Print json with all files found if copy is confirmed
    #[arg(long, short = 'f')]
    print_found: bool,

    /// Disable progress bar
    #[arg(long, default_value_t = false)]
    no_progress_bar: bool,

    /// Exclude pattern from being compared from src directory
    #[arg(long)]
    exclude: Vec<String>,

    /// Print all files excluded from comparison to this file ("-" for stderr)
    #[arg(long)]
    print_excluded: Option<OsString>,
}

fn main() -> Result<(), ConfirmerError> {
    env_logger::init();

    let args = Args::parse();

    let num_threads = max(1, args.jobs);

    let mut cc = match args.no_progress_bar {
        true => CopyConfirmer::new(num_threads),
        false => CopyConfirmer::new(num_threads).with_progress_bar(),
    };

    for mut path in args.exclude {
        let pattern = if path.starts_with("/") {
            let mut full_path = PathBuf::from(args.source.clone());
            // Remove the leading slash - otherwise whole path gets replaced by `path`
            path.remove(0);
            full_path.push(path);
            ExcludePattern::MatchPathStart(
                full_path
                    .into_os_string()
                    .into_string()
                    .expect("Badly formed source string or exclude string"),
            )
        } else {
            ExcludePattern::MatchEverywhere(path)
        };
        cc = cc.add_excluded_pattern(pattern);
    }

    match cc.compare(args.source, &args.destination)? {
        ConfirmerResult::Ok(filelist) => {
            println!("All files present in destinations.");
            if args.print_found {
                let files_found = serde_json::to_string_pretty(&filelist).unwrap();

                if let Some(out_file) = args.out_file {
                    let mut file = File::create(out_file)?;
                    file.write_all(&files_found.into_bytes())?;
                } else {
                    println!("{files_found}");
                }
            }
        }
        ConfirmerResult::MissingFiles(files) => {
            println!("{}", "Missing files:".red().bold());
            for file in files {
                println!("{file:?}");
            }
        }
    }

    let stderr_output_str = OsString::from("-");
    match args.print_excluded {
        Some(string) if string == stderr_output_str => {
            println!("{}", "Excluded files:".red().bold());
            println!("{:?}", cc.get_excluded_paths());
        }
        Some(out_file) => {
            let mut file = File::create(out_file)?;
            for f in cc.get_excluded_paths() {
                write!(file, "{f:?}\n");
            }
        }
        None => {}
    }
    Ok(())
}
