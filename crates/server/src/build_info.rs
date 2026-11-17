//! What this binary is, baked in at compile time (ticket T20a).
//!
//! `/healthz` reports both, because "the pod is up" and "the pod is the release
//! I just deployed" are different questions and only the second one catches a
//! rollout that silently kept the old image.

/// The crate version, i.e. what `Cargo.toml` says.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// The short commit this binary was built from, or `unknown` when it was built
/// from a tree with no git history to ask (see `build.rs`).
pub const COMMIT: &str = env!("STRANDS_GIT_COMMIT");
