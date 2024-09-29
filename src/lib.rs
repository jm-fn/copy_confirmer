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
//!
//! We can show a progress bar by setting [with_progress_bar](CopyConfirmer::with_progress_bar). We
//! can exclude files from comparison with
//! [add_excluded_pattern](CopyConfirmer::add_excluded_pattern).

mod checksum;
mod copcon_error;

use std::cell::Cell;
use std::collections::HashMap;
use std::ffi::{OsStr, OsString};
use std::io::Result as IoResult;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::{thread, time};

use indicatif::{ProgressBar, ProgressStyle};
use threadpool::ThreadPool;
use walkdir::WalkDir;

use checksum::*;
pub use copcon_error::ConfirmerError;
use log;
use serde::{ser::SerializeSeq, Serialize, Serializer};

/// Indicates whether there are files missing in destination dirs
#[derive(Debug, PartialEq)]
pub enum ConfirmerResult {
    /// Indicates all files in source are in at least one destination dir
    ///
    /// Contains HashMap with key ~ checksum of a file and value ~ [FileFound](FileFound) struct
    /// that contains files corresponding to that checksum in source and destination directories.
    Ok(HashMap<String, FileFound>),
    /// Contains files in source that are missing from all destinations
    MissingFiles(Vec<OsString>),
}

/// Holds information on all paths in source and destinations that contain the same file
#[derive(Debug, PartialEq, Serialize)]
pub struct FileFound {
    /// Paths of same files in source
    #[serde(serialize_with = "osstring_serialize")]
    pub src_paths: Vec<OsString>,
    /// Paths of same files in destinations
    #[serde(serialize_with = "osstring_serialize")]
    pub dest_paths: Vec<OsString>,
}

/// Exclude pattern
///
/// The paths in source directory are matched with the pattern. If the path contains the pattern
/// string, it is excluded from the comparison.
pub enum ExcludePattern {
    /// Compare string is anchored to the root of the source directory
    ///
    /// Matches only paths starting with contents of MatchPathStart.
    ///
    /// Note that the content string should be in form <source_dir + /path/to/sth>
    MatchPathStart(String),
    /// All paths containing the string are matched
    MatchEverywhere(String),
    // TODO: Add MatchPathFromSource or sth that anchors the contents to source_dir? This would
    // work kinda similarly to MatchPathStart
    // TODO: Add wildcards?
}

/// Helper function for serialisation of paths
fn osstring_serialize<S>(hs: &Vec<OsString>, s: S) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    let mut seq = s.serialize_seq(Some(hs.len()))?;
    for item in hs.iter() {
        let stringy: String = item
            .to_owned()
            .into_string()
            .unwrap_or_else(|osstr| format!("Error decoding this: {:?}", osstr));
        seq.serialize_element(&stringy)?;
    }
    seq.end()
}

/// type for mpsc channel in CopyConfirmer
type HashResult = (OsString, IoResult<String>);

/// Time period for checking the threadpool status
const HUNDRED_MILIS: time::Duration = time::Duration::from_millis(100);

/// Structure providing methods for directory comparison
pub struct CopyConfirmer {
    hashes_tx: Sender<HashResult>,
    hashes_rx: Receiver<HashResult>,
    threadpool: ThreadPool,
    show_progress: bool,
    excluded_pattern: Vec<ExcludePattern>,
    excluded_paths: Cell<Vec<OsString>>,
}

impl CopyConfirmer {
    /// Initiate new `CopyConfirmer`
    ///
    /// # Arguments
    /// * `num_threads` - number of jobs for checksum calculation to be run in parallel
    pub fn new(num_threads: usize) -> Self {
        let (hashes_tx, hashes_rx) = channel();
        let threadpool = ThreadPool::new(num_threads);
        Self {
            hashes_tx,
            hashes_rx,
            threadpool,
            show_progress: false,
            excluded_pattern: vec![],
            excluded_paths: Cell::new(vec![]),
        }
    }

    /// Enable progress bar
    pub fn with_progress_bar(self) -> Self {
        Self {
            hashes_tx: self.hashes_tx,
            hashes_rx: self.hashes_rx,
            threadpool: self.threadpool,
            show_progress: true,
            excluded_pattern: self.excluded_pattern,
            excluded_paths: self.excluded_paths,
        }
    }

    /// Add exclude pattern
    ///
    /// The pattern is matched against the paths contained in source directory and the matching
    /// paths get excluded from comparison entirely. (So that they are not reported when they are
    /// missing.)
    ///
    /// The method can be used multiple times to exclude multiple patterns.
    ///
    /// Use method (get_excluded_paths)[CopyConfirmer::get_excluded_paths] to get all files excluded by CopyConfirmer.
    pub fn add_excluded_pattern(self, exclude: ExcludePattern) -> Self {
        let mut modifiable = self;
        modifiable.excluded_pattern.push(exclude);
        modifiable
    }

    /// Check if all files in source are also in one of destinations
    ///
    /// Returns `ConfirmerResult::Ok` if all files in `source` directory are in at least one
    /// directory in `destinations`. Returns `ConfirmerResult::MissingFiles()`
    ///
    /// # Arguments
    /// * `source` - path to the source directory
    /// * `destinations` - vector of paths of destination directories
    pub fn compare<T: AsRef<OsStr>>(
        &self,
        source: T,
        destinations: &[T],
    ) -> Result<ConfirmerResult, ConfirmerError> {
        // Total numbers of files for progress bars
        let source: &OsStr = source.as_ref();
        let mut excluded_files: Vec<OsString> = vec![];
        let destinations: Vec<&OsStr> = destinations.iter().map(|x| x.as_ref()).collect();
        let total_files_source = get_total_files(source);
        let total_dest_files: u64 = destinations.iter().map(|x| get_total_files(x)).sum();

        // Keys = hashes of files in source dir, values = vectors of paths to files with the hash
        let mut missing_files: HashMap<String, Vec<OsString>> = HashMap::new();
        // hash map for Ok result
        let mut found_files: HashMap<String, FileFound> = HashMap::new();

        self._enqueue_all_hashes_src(source, &mut excluded_files)?;

        // To reduce total files count in progress
        let excluded_count = excluded_files.len();
        // Add excluded files to self, so that it can be exported
        let mut ex_paths = self.excluded_paths.take();
        ex_paths.append(&mut excluded_files);
        self.excluded_paths.set(ex_paths);

        self._track_progress(
            total_files_source - excluded_count as u64,
            "Checking files from source",
        );

        // Return Error on any panic
        if self.threadpool.panic_count() > 0 {
            return Err(ConfirmerError("A panic occured while calculating hashes.".into()));
        }
        // Add hashes for all files found in source dir to `missing files`
        for result in self.hashes_rx.try_iter() {
            match result {
                (path, Ok(hash)) => {
                    // FIXME: do this without cloning
                    // Append if there is already an entry with the same hash
                    missing_files
                        .entry(hash)
                        .and_modify(|vec| vec.push(path.clone()))
                        .or_insert(vec![path]);
                }
                (path, Err(e)) => {
                    eprintln!("Error getting hash {:?}: {}", path, e);
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
            return Err(ConfirmerError("A panic occured while calculating hashes.".into()));
        }

        // Remove all files found in destinations from `missing_files`
        for result in self.hashes_rx.try_iter() {
            match result {
                (dest_path, Ok(hash)) => {
                    if let Some(src_paths) = missing_files.remove(&hash) {
                        found_files
                            .entry(hash)
                            .and_modify(|FileFound { dest_paths, .. }| {
                                dest_paths.push(dest_path.clone())
                            })
                            .or_insert(FileFound { src_paths, dest_paths: vec![dest_path] });
                    }
                }
                (dest_path, Err(e)) => {
                    eprintln!("Error getting hash {:?}: {}", dest_path, e);
                    return Err(e.into());
                }
            }
        }

        // Return all files left in `missing_files` or `Ok`
        if missing_files.is_empty() {
            Ok(ConfirmerResult::Ok(found_files))
        } else {
            Ok(ConfirmerResult::MissingFiles(missing_files.into_values().flatten().collect()))
        }
    }

    /// Get paths of files that were excluded from comparison
    ///
    /// See [add_excluded_pattern](CopyConfirmer::add_excluded_pattern) and [ExcludePattern].
    pub fn get_excluded_paths(&self) -> Vec<OsString> {
        let ex_paths = self.excluded_paths.take();
        let result = ex_paths.clone();
        self.excluded_paths.set(ex_paths);
        result
    }

    /// Go recursively through directory. For each file add a job to calculate its checksum to the
    /// threadpool.
    ///
    /// Returns std::io::Error if any path cannot be accessed
    ///
    /// # Arguments
    /// * `dir` - directory to go through and get all hashes
    fn _enqueue_all_hashes(&self, dir: &OsStr) -> IoResult<()> {
        for item in WalkDir::new(dir) {
            let item = item?;
            if !item.file_type().is_file() {
                continue;
            }
            let path = item.into_path().into_os_string();
            let sender = self.hashes_tx.clone();
            self.threadpool.execute(move || {
                sender
                    .send((path.clone(), get_hash(path)))
                    .expect("Could not send source file hash")
            });
        }
        Ok(())
    }

    /// Go recursively through directory. For each file add a job to calculate its checksum to the
    /// threadpool. Does not process the directories/files that match excluded patterns given.
    ///
    /// Returns std::io::Error if any path cannot be accessed
    ///
    /// # Arguments
    /// * `dir` - directory to go through and get all hashes
    fn _enqueue_all_hashes_src(
        &self,
        dir: &OsStr,
        excluded_files: &mut Vec<OsString>,
    ) -> IoResult<()> {
        for item in WalkDir::new(dir) {
            let item = item?;
            if !item.file_type().is_file() {
                continue;
            }
            let path = item.into_path().into_os_string();

            // Filter out excluded patterns
            if !self.excluded_pattern.is_empty() && is_path_excluded(&path, &self.excluded_pattern)
            {
                excluded_files.push(path);
                continue;
            }

            let sender = self.hashes_tx.clone();
            self.threadpool.execute(move || {
                sender
                    .send((path.clone(), get_hash(path)))
                    .expect("Could not send source file hash")
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
        let mut pbar: Option<ProgressBar> = None;
        if self.show_progress {
            let pb_style = ProgressStyle::with_template(
                "[{elapsed_precise}] {bar:40.cyan/blue} {pos:>7}/{len:7} {msg}",
            )
            .unwrap()
            .progress_chars("##-");
            pbar = Some(ProgressBar::new(total_files).with_style(pb_style));
            pbar.as_ref().unwrap().set_message(msg);
        }

        let mut num_not_done = self.threadpool.active_count() + self.threadpool.queued_count();
        while num_not_done > 0 {
            num_not_done = self.threadpool.active_count() + self.threadpool.queued_count();
            if self.show_progress {
                pbar.as_ref().unwrap().set_position(total_files - num_not_done as u64);
            }
            thread::sleep(2 * HUNDRED_MILIS);
        }
        if self.show_progress {
            pbar.as_ref().unwrap().finish();
        }
    }
}

/// Get number of files in directory
fn get_total_files(dir: &OsStr) -> u64 {
    WalkDir::new(dir)
        .follow_root_links(false)
        .into_iter()
        .filter_map(|x| x.ok())
        .filter(|x| x.file_type().is_file())
        .count() as u64
}

/// Get tuple of hash and path
fn get_hash(path: OsString) -> IoResult<String> {
    let checksum = get_blake2_checksum(&path)?;
    Ok(checksum)
}

/// Returns true if path contains one of excluded patterns
fn is_path_excluded(path: &OsStr, excluded_patterns: &Vec<ExcludePattern>) -> bool {
    use ExcludePattern::*;
    let path_str = path.to_str().expect("Could not decode path string.");
    for pattern in excluded_patterns {
        match pattern {
            MatchEverywhere(part) => {
                if path_str.contains(part) {
                    return true;
                }
            }

            MatchPathStart(part) => {
                if path_str.starts_with(part) {
                    return true;
                }
            }
        }
    }

    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn test_exclusion_match_path_start() -> Result<(), ConfirmerError> {
        // Excludes foo subdir
        let excluded_pattern_1 =
            ExcludePattern::MatchPathStart(String::from("tests/fixtures/exclusion/dir_A/foo"));
        // Will not exclude anything
        let excluded_pattern_2 = ExcludePattern::MatchPathStart(String::from("/bar"));
        let cc = CopyConfirmer::new(1)
            .add_excluded_pattern(excluded_pattern_1)
            .add_excluded_pattern(excluded_pattern_2);
        let result = cc.compare(
            String::from("tests/fixtures/exclusion/dir_A"),
            &[String::from("tests/fixtures/exclusion/dir_B")],
        )?;

        let expected_missing = vec!["tests/fixtures/exclusion/dir_A/bar/foo.txt".into()];
        assert_eq!(result, ConfirmerResult::MissingFiles(expected_missing));

        let excluded = cc.get_excluded_paths();
        let expected_excluded: Vec<OsString> = vec![
            "tests/fixtures/exclusion/dir_A/foo/bar.txt".into(),
            "tests/fixtures/exclusion/dir_A/foo/baz.txt".into(),
            "tests/fixtures/exclusion/dir_A/foo/foo.txt".into(),
        ];
        assert_eq!(
            HashSet::<OsString>::from_iter(excluded.into_iter()),
            HashSet::from_iter(expected_excluded.into_iter())
        );
        Ok(())
    }

    #[test]
    fn test_exclusion_match_everywhere() -> Result<(), ConfirmerError> {
        // Excludes foo subdir
        let excluded_pattern = ExcludePattern::MatchEverywhere(String::from("bar"));
        // Will not exclude anything
        let cc = CopyConfirmer::new(1).add_excluded_pattern(excluded_pattern);
        let result = cc.compare(
            String::from("tests/fixtures/exclusion/dir_A"),
            &[String::from("tests/fixtures/exclusion/dir_B")],
        )?;

        let expected_missing = vec!["tests/fixtures/exclusion/dir_A/foo/foo.txt".into()];
        assert_eq!(result, ConfirmerResult::MissingFiles(expected_missing));

        let excluded = cc.get_excluded_paths();
        let expected_excluded: Vec<OsString> = vec![
            "tests/fixtures/exclusion/dir_A/foo/bar.txt".into(),
            "tests/fixtures/exclusion/dir_A/bar/foo.txt".into(),
        ];
        assert_eq!(
            HashSet::<OsString>::from_iter(excluded.into_iter()),
            HashSet::from_iter(expected_excluded.into_iter())
        );
        Ok(())
    }
}
