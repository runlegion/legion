// Emit LEGION_BUILD_ID at compile time so the running daemon and the local
// binary can be compared beyond their semver version (#698).
//
// The daemon supervisor restarts on version drift, but a rebuild that does
// NOT bump Cargo.toml -- the common dev case -- keeps the same version and
// would otherwise go unnoticed (reflection 019f1565). The build id is the
// git short SHA, with a `-dirty` suffix when the working tree has changes,
// and `unknown` when git is unavailable (e.g. a crates.io install). The
// supervisor treats `unknown` as "cannot tell" and falls back to version-
// only comparison, so a missing id never causes a restart loop.
//
// LIMITATION: successive *dirty* rebuilds at the same HEAD yield the same
// `<sha>-dirty` id, so the supervisor will not catch them. The target case
// is the committed rebuild; dirty iteration is expected to restart by hand.
// Note `git status --porcelain` also reports untracked files, so a stray
// scratch file marks the build `-dirty` even when tracked state is clean --
// harmless given the by-hand stance, but it widens the dirty surface.
use std::process::Command;

fn main() {
    let build_id = git_build_id().unwrap_or_else(|| "unknown".to_string());
    println!("cargo:rustc-env=LEGION_BUILD_ID={build_id}");

    // logs/HEAD is appended on every HEAD movement (commit, checkout,
    // reset), so watching it reruns build.rs -- and refreshes the id --
    // whenever the committed state changes. (Plain .git/HEAD does not
    // change when a new commit lands on the current branch.)
    println!("cargo:rerun-if-changed=.git/logs/HEAD");
}

fn git_build_id() -> Option<String> {
    let sha_out = Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .output()
        .ok()?;
    if !sha_out.status.success() {
        return None;
    }
    let sha = String::from_utf8(sha_out.stdout).ok()?.trim().to_string();
    if sha.is_empty() {
        return None;
    }

    let dirty = Command::new("git")
        .args(["status", "--porcelain"])
        .output()
        .ok()
        .map(|o| !o.stdout.is_empty())
        .unwrap_or(false);

    Some(if dirty { format!("{sha}-dirty") } else { sha })
}
