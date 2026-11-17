//! Stamps the commit the binary was built from into the binary (ticket T20a),
//! so `/healthz` can name the release it is actually running and a rollout can
//! be checked against the tag it claims to be.
//!
//! Two sources, in order: `STRANDS_GIT_COMMIT` from the environment (the image
//! build passes it, because `.dockerignore` keeps `.git` out of the context),
//! then `git rev-parse` for a checkout built by hand. `unknown` is the honest
//! answer when neither exists — never a guess.

use std::path::Path;
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-env-changed=STRANDS_GIT_COMMIT");

    // A checkout built by hand should report the commit it is on, so the
    // script re-runs when HEAD moves. Absent (an exported tree, the image
    // build) there is nothing to watch and no rebuild to force.
    let head = Path::new("../../.git/HEAD");
    if head.exists() {
        println!("cargo:rerun-if-changed=../../.git/HEAD");
    }

    // Shortened to twelve characters whichever source it came from: the image
    // build is handed the full forty, a hand build already has seven, and a
    // reader of `/healthz` should not have to care which one they are looking
    // at. Twelve is still far more than enough to identify a commit.
    let commit = std::env::var("STRANDS_GIT_COMMIT")
        .ok()
        .map(|value| value.trim().chars().take(12).collect::<String>())
        .filter(|value: &String| !value.is_empty())
        .or_else(from_git)
        .unwrap_or_else(|| "unknown".to_owned());

    println!("cargo:rustc-env=STRANDS_GIT_COMMIT={commit}");
}

fn from_git() -> Option<String> {
    let output = Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let sha = String::from_utf8(output.stdout).ok()?.trim().to_owned();
    (!sha.is_empty()).then_some(sha)
}
