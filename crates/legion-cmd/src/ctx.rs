//! Everything the router is allowed to know, as plain owned data (FR-CMD-001).
//!
//! `Ctx` has NO lifetime parameter and holds no handles. That is the shape of
//! the purity guarantee: the router cannot reach a database or a filesystem
//! through its context because there is nothing in here to reach through. The
//! caller does the I/O and hands over the result.

use serde::{Deserialize, Serialize};

/// A ruling the router matches against a call.
///
/// MINIMAL PLACEHOLDER (slice 1). The real match-spec -- a list of specs each
/// carrying tool, `path_glob_not` / `content_regex_not`, `any_input`, and a hit
/// slot -- is FR-CMD-017's job in slice 7. `Ctx` needs a concrete type to
/// compile, and inventing the predicate system here would pre-empt the issue
/// that owns it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Ruling {
    pub id: String,
    pub tool: Option<String>,
    pub summary: String,
}

/// A recall result pre-loaded for the call (FR-CMD-014).
///
/// MINIMAL PLACEHOLDER (slice 1). No accepted legion-cmd document specifies
/// this shape, and legion's own recall result carries substantially more than
/// a router could use. The fields here are the ones every recall surface
/// already prints; whichever slice first reads real fields off this type
/// (slice 8) should replace it deliberately rather than grow it by accident.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct RecallHit {
    pub id: String,
    pub text: String,
    pub score: f64,
}

/// The router's whole world.
#[derive(Debug, Clone, Default)]
pub struct Ctx {
    pub repo: Option<String>,
    pub cwd: Option<String>,
    /// `LEGION_*` keys only -- the router never sees the caller's full
    /// environment, so it cannot branch on something the record does not show.
    pub env: std::collections::BTreeMap<String, String>,
    pub index_present: bool,
    pub allow_list: Vec<String>,
    pub rulings: Vec<Ruling>,
    pub recall: Vec<RecallHit>,
    /// Line count of the file this call reads, when the caller resolved one.
    ///
    /// The router does no I/O (FR-CMD-001), so a module that decides on file
    /// size cannot stat the file itself -- the caller counts and hands the
    /// number over, the same arrangement as `rulings` and `recall`. `None`
    /// means "not resolved", which is not the same as zero and must not be
    /// read as a small file.
    pub file_lines: Option<u64>,
    /// Whether a Grep/Glob call's symbol candidate resolves to a `legion sym
    /// def` hit local to `Ctx.repo`.
    ///
    /// MINIMAL PLACEHOLDER (#1051), same shape as `file_lines`: the router
    /// does no I/O, so `pre_grep` cannot run `legion sym def` itself -- the
    /// caller probes it and folds the result down to this one bit before
    /// handing `Ctx` over. `false` covers "no hit", "hit exists only in
    /// other repos" (the #458 relevance gate a cluster-wide hit must not
    /// trip), and "not yet resolved" alike; only a caller-confirmed LOCAL
    /// hit sets this `true`. The full hit payload (for embedding the actual
    /// `legion sym def` JSON in a deny reason) is a real gap this field does
    /// not close -- see `pre_grep`'s module docs.
    pub sym_local_hit: bool,
}
