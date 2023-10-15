//! Directory comparison library
//!
//! Compares directories (`source` and multiple dirs in `destinations`) by creating hash of each
//! file in `source` and then checking that there is at least one file with the same hash in one of
//! directories in `destinations`. If all files in `source` are in at least one of the destination
//! directories, we return `ConfirmerResult::Ok`, otherwise we list all the missing files in
//! `ConfirmerResult::MissingFiles`.
//!
//! # Example usage
//! Suppose we have a directory structure:
//! ``` bash
//! tests/fixtures/
//! ├── dir_A
//! │   ├── bar.txt
//! │   └── foo.txt
//! └── dir_B
//!     └── foo.txt
//! ```
//! We can use copy confirmer to confirm that `dir_B` is a copy of `dir_A`:
//! ```
//! use copy_confirmer::*;
//!
//! # fn main() -> Result<(), ConfirmerError> {
//! let cc = CopyConfirmer::new(1);
//! let missing_files = cc.compare("tests/fixtures/dir_A",
//!                                &["tests/fixtures/dir_B"])?;
//!
//! let expected_missing = vec!["tests/fixtures/dir_A/bar.txt".into()];
//! assert_eq!(missing_files, ConfirmerResult::MissingFiles(expected_missing));
//! # Ok(())
//! # }
//! ```

mod checksum;
mod copcon_error;

use std::collections::HashMap;
use std::ffi::{OsStr, OsString};
use std::io::Result as IoResult;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::{thread, time};
use std::fmt::Display;

use indicatif::{ProgressBar, ProgressStyle};
use threadpool::ThreadPool;
use walkdir::WalkDir;

use checksum::*;
pub use copcon_error::ConfirmerError;
use log;

/// Indicates whether there are files missing in destination dirs
#[derive(Debug, PartialEq)]
pub enum ConfirmerResult {
    /// Indicates all files in source are in at least one destination dir
    Ok,
    /// Contains files in source that are missing from all destinations
    MissingFiles(Vec<OsString>),
}


/// type for mpsc channel in CopyConfirmer
type HashResult = IoResult<(String, OsString)>;

/// Time period for checking the threadpool status
const HUNDRED_MILIS: time::Duration = time::Duration::from_millis(100);

/// Structure providing methods for directory comparison
pub struct CopyConfirmer {
    hashes_tx: Sender<HashResult>,
    hashes_rx: Receiver<HashResult>,
    threadpool: ThreadPool,
}

impl CopyConfirmer {
    /// Initiate new `CopyConfirmer`
    ///
    /// # Arguments
    /// * `num_threads` - number of jobs for checksum calculation to be run in parallel
    pub fn new(num_threads: usize) -> Self {
        let (hashes_tx, hashes_rx) = channel();
        let threadpool = ThreadPool::new(num_threads);
        Self { hashes_tx, hashes_rx, threadpool }
    }

    /// Check if all files in source are also in one of destinations
    ///
    /// Returns `ConfirmerResult::Ok` if all files in `source` directory are in at least one
    /// directory in `destinations`. Returns `ConfirmerResult::MissingFiles()`
    ///
    /// # Arguments
    /// * `source` - path to the source directory
    /// * `destinations` - vector of paths of destination directories
    pub fn compare<T: AsRef<OsStr>>(&self, source: T, destinations: &[T]) -> Result<ConfirmerResult, ConfirmerError> {
        // Total numbers of files for progress bars
        let source: &OsStr = source.as_ref();
        let destinations: Vec<&OsStr> = destinations.iter().map(|x| x.as_ref()).collect();
        let total_files_source = get_total_files(source);
        let total_dest_files: u64 = destinations.iter().map(|x| get_total_files(x)).sum();

        // Keys = hashes of files in source dir, values = vectors of paths to files with the hash
        let mut missing_files: HashMap<String, Vec<OsString>> = HashMap::new();

        self._enqueue_all_hashes(&source)?;

        self._track_progress(total_files_source, "Checking files from source");

        // Return Error on any panic
        if self.threadpool.panic_count() > 0 {
            return Err(ConfirmerError(
                "A panic occured while calculating hashes.".into(),
            ));
        }
        // Add hashes for all files found in source dir to `missing files`
        for result in self.hashes_rx.try_iter() {
            match result {
                Ok((hash, path)) => {
                    // FIXME: do this without cloning
                    // Append if there is already an entry with the same hash
                    missing_files
                        .entry(hash)
                        .and_modify(|vec| vec.push(path.clone()))
                        .or_insert(vec![path]);
                }
                Err(e) => {
                    eprintln!("Error getting hash: {e}");
                    return Err(e.into());
                }
            }
        }

        // Get hashes for all files in destinations
        for dest in destinations {
            self._enqueue_all_hashes(dest)?;
        }

        // FIXME: Would be better to use the results continually instead of waiting for all hashes
        // and return early once missing_files is empty, since destinations dirs can be
        // significantly larger than source dir
        self._track_progress(total_dest_files, "Checking files from destinations");

        // Return Error on any panic
        if self.threadpool.panic_count() > 0 {
            return Err(ConfirmerError(
                "A panic occured while calculating hashes.".into(),
            ));
        }

        // Remove all files found in destinations from `missing_files`
        for result in self.hashes_rx.try_iter() {
            match result {
                Ok((hash, _path)) => {
                    missing_files.remove(&hash);
                }
                Err(e) => {
                    eprintln!("Error getting hash: {e}");
                    return Err(e.into());
                }
            }
        }

        // Return all files left in `missing_files` or `Ok`
        if missing_files.len() == 0 {
            Ok(ConfirmerResult::Ok)
        } else {
            Ok(ConfirmerResult::MissingFiles(missing_files.into_values().flatten().collect()))
        }
    }

    /// Go recursively through directory. For each file add a job to calculate its checksum to the
    /// threadpool.
    ///
    /// Returns std::io::Error if any path cannot be accessed
    ///
    /// # Arguments
    /// * `dir` - directory to go through and get all hashes
    fn _enqueue_all_hashes(&self, dir: &OsStr) -> IoResult<()> {
        for item in WalkDir::new(&dir) {
            let item = item?;
            if item.file_type().is_dir() {
                continue;
            }
            let path = item.into_path().into_os_string();
            let sender = self.hashes_tx.clone();
            self.threadpool.execute(move || {
                sender.send(get_hash(path)).expect("Could not send source file hash")
            });
        }
        Ok(())
    }

    /// Print progress bar that tracks progress on getting hashes of files
    ///
    /// # Arguments
    /// * `total_files` - number of files enqueued in the threadpool for calculation of hash
    /// * `msg` - message to print with progress bar
    fn _track_progress(&self, total_files: u64, msg: &'static str) {
        let pb_style = ProgressStyle::with_template(
            "[{elapsed_precise}] {bar:40.cyan/blue} {pos:>7}/{len:7} {msg}",
        )
        .unwrap()
        .progress_chars("##-");
        let pbar = ProgressBar::new(total_files).with_style(pb_style);
        pbar.set_message(msg);

        let mut num_not_done = self.threadpool.active_count() + self.threadpool.queued_count();
        while num_not_done > 0 {
            num_not_done = self.threadpool.active_count() + self.threadpool.queued_count();
            pbar.set_position(total_files - num_not_done as u64);
            log::info!("Tracking progress.");
            thread::sleep(2*HUNDRED_MILIS);
        }
        pbar.finish();
    }
}

/// Get number of files in directory
fn get_total_files(dir: &OsStr) -> u64 {
    WalkDir::new(dir).into_iter().filter_map(|x| x.ok()).filter(|x| !x.file_type().is_dir()).count()
        as u64
}

/// Get tuple of hash and path
fn get_hash(path: OsString) -> IoResult<(String, OsString)> {
    let checksum = get_blake2_checksum(&path)?;
    Ok((checksum, path))
}
