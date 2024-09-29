use std::ffi::OsStr;
/// Checksum calculation module
///
/// Contains all functions for various checksum calculation.
use std::fs::File;
use std::io::Result as IoResult;
use std::io::{prelude::Read, BufReader};

use blake2::{Blake2b512, Digest};

/// Calculate checksum for a whole file
///
/// # Arguments
/// * `path` - path to the file to be checksummed
pub(crate) fn get_blake2_checksum(path: &OsStr) -> IoResult<String> {
    let mut hasher = Blake2b512::new();
    let mut buffer = [0u8; 1024];

    let mut buf_reader = BufReader::new(File::open(path)?);

    loop {
        let count = buf_reader.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        hasher.update(&buffer[..count]);
    }

    let result = format!("{:x}", hasher.finalize());
    Ok(result)
}
