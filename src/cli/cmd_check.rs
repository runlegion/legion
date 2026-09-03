//! `legion cmd-check` (FR-CMD-008): the sole CLI entry point into the
//! legion-cmd router. Parses `--tool`/`<input>`, builds `Ctx` the same way
//! the future PreToolUse adapter (slice 9) will, and hands both to
//! `legion_cmd::cmd_check` -- routing itself lives entirely in
//! `legion_cmd::Router::route`, so this module owns no decision logic.
//!
//! `Ctx` construction (env read, cwd read) happens HERE, not inside the
//! `legion-cmd` crate: `legion_cmd::Ctx`'s doc comment is explicit that the
//! router does no I/O, so the caller gathers what it needs and hands over
//! the result.

use std::collections::BTreeMap;

use legion_cmd::{Ctx, RouteTable, Router, cmd_check, format_plain};

use crate::error;

/// Build the `Ctx` `cmd-check` routes against: `--repo`, the process cwd,
/// and only `LEGION_*` environment keys (`Ctx.env`'s doc comment -- the
/// router never sees the caller's full environment). `rulings`, `recall`,
/// and `file_lines` stay empty: pre-load recall (slice 8) and the ruling
/// match-spec (slice 7) are later slices' jobs, not this verb's.
fn build_ctx(repo: Option<String>) -> Ctx {
    let cwd = std::env::current_dir()
        .ok()
        .and_then(|p| p.to_str().map(str::to_owned));
    let env: BTreeMap<String, String> = std::env::vars()
        .filter(|(key, _)| key.starts_with("LEGION_"))
        .collect();
    Ctx {
        repo,
        cwd,
        env,
        ..Ctx::default()
    }
}

pub(crate) fn handle_cmd_check(
    tool: String,
    repo: Option<String>,
    json: bool,
    input: String,
) -> error::Result<()> {
    let router = Router::new(RouteTable::embedded()?)?;
    let ctx = build_ctx(repo);

    let output = cmd_check(&router, &ctx, &tool, &input)?;

    if json {
        println!("{}", serde_json::to_string(&output)?);
    } else {
        print!("{}", format_plain(&output));
    }
    Ok(())
}
