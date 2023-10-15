//! Module with CopyConfirmer-specific error

use std::fmt::Display;
use std::io;

/// An error produced when comparing directories
#[derive(Debug)]
pub struct ConfirmerError(pub String);

impl Display for ConfirmerError {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl From<io::Error> for ConfirmerError {
    fn from(error: io::Error) -> Self {
        ConfirmerError(error.to_string())
    }
}
