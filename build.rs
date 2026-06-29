// This build script does two independent jobs:
//
// 1. Emit LEGION_BUILD_ID at compile time so the running daemon and the local
//    binary can be compared beyond their semver version (#698).
//
//    The daemon supervisor restarts on version drift, but a rebuild that does
//    NOT bump Cargo.toml -- the common dev case -- keeps the same version and
//    would otherwise go unnoticed (reflection 019f1565). The build id is the
//    git short SHA, with a `-dirty` suffix when the working tree has changes,
//    and `unknown` when git is unavailable (e.g. a crates.io install). The
//    supervisor treats `unknown` as "cannot tell" and falls back to version-
//    only comparison, so a missing id never causes a restart loop.
//
//    LIMITATION: successive *dirty* rebuilds at the same HEAD yield the same
//    `<sha>-dirty` id, so the supervisor will not catch them. The target case
//    is the committed rebuild; dirty iteration is expected to restart by hand.
//    Note `git status --porcelain` also reports untracked files, so a stray
//    scratch file marks the build `-dirty` even when tracked state is clean --
//    harmless given the by-hand stance, but it widens the dirty surface.
//
// 2. Ensure the embedded dashboard directory exists before rust-embed reads it
//    (#697).
//
//    The real assets come from `pnpm -C app build` (vite -> app/dist), which CI
//    and releases run before `cargo build` so the binary embeds the current
//    dashboard. But a plain `cargo build` / `cargo install` on a machine without
//    Node must still succeed -- so if app/dist is missing we drop a placeholder
//    index.html. This keeps the Rust build Node-free while the frontend build
//    stays a separate, opt-in step.
use std::fs;
use std::path::Path;
use std::process::Command;

fn main() {
    // (1) build id
    let build_id = git_build_id().unwrap_or_else(|| "unknown".to_string());
    println!("cargo:rustc-env=LEGION_BUILD_ID={build_id}");

    // logs/HEAD is appended on every HEAD movement (commit, checkout,
    // reset), so watching it reruns build.rs -- and refreshes the id --
    // whenever the committed state changes. (Plain .git/HEAD does not
    // change when a new commit lands on the current branch.)
    println!("cargo:rerun-if-changed=.git/logs/HEAD");

    // (2) embedded dashboard placeholder
    let dist = Path::new("app/dist");
    let index = dist.join("index.html");
    if !index.exists() {
        if let Err(e) = fs::create_dir_all(dist) {
            println!("cargo:warning=could not create app/dist: {e}");
            return;
        }
        let placeholder = "<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\">\
<title>legion</title></head><body><p>Dashboard not built. \
Run: <code>pnpm -C app build</code></p></body></html>";
        if let Err(e) = fs::write(&index, placeholder) {
            println!("cargo:warning=could not write app/dist placeholder: {e}");
        }
    }
    // Rebuild when the built dashboard changes so the embed stays current.
    println!("cargo:rerun-if-changed=app/dist");
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
