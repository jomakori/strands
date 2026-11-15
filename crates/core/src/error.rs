//! Domain validation errors shared by core types.

use thiserror::Error;

/// Failure modes when constructing a domain type from untrusted input.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum ValidationError {
    #[error("body must not be empty")]
    EmptyBody,
    #[error("name must not be empty")]
    EmptyName,
}
