# Phase 3.0: Synapse Quality Gate Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Add LLM-powered quality gating (VALIDATE) and auto-classification (CLASSIFY) to reflections via the Anthropic API (Sonnet), opt-in via `--synapse` flag.

**Architecture:** New `synapse.rs` module handles API calls to Claude Sonnet. Two primitives: VALIDATE (accept/reject with reason) and CLASSIFY (domain, tags, transfer flag, specificity score). Called at write time (reflect/post), not read time. Fail-open: API errors log a warning and the reflection is stored anyway. Board posts bypass validation but still get classified.

**Tech Stack:** reqwest (rustls-tls), serde_json (already present), Anthropic Messages API

---

### Task 1: Add reqwest dependency

**Files:**
- Modify: `Cargo.toml`

**Step 1: Add reqwest to dependencies**

Add after the `model2vec-rs` line in `[dependencies]`:

```toml
reqwest = { version = "0.12", features = ["rustls-tls", "json"], default-features = false }
tokio = { version = "1", features = ["rt", "macros"] }
```

We need tokio because reqwest is async. We'll use `tokio::runtime::Runtime` to block on async calls from our sync main.

**Step 2: Verify it compiles**

Run: `cargo check`
Expected: compiles with new dependencies

**Step 3: Commit**

```bash
git add Cargo.toml Cargo.lock
git commit -m "chore: add reqwest and tokio dependencies for Synapse API calls"
```

---

### Task 2: Add Synapse error variant and API types

**Files:**
- Modify: `src/error.rs`
- Create: `src/synapse.rs`

**Step 1: Write the failing test for SynapseError**

In `src/error.rs`, add to the test module:

```rust
#[test]
fn error_display_synapse() {
    let err = LegionError::Synapse("API rate limited".to_string());
    assert_eq!(err.to_string(), "synapse error: API rate limited");
}
```

**Step 2: Run test to verify it fails**

Run: `cargo test error::tests::error_display_synapse`
Expected: FAIL - no variant named `Synapse`

**Step 3: Add the Synapse variant to LegionError**

In `src/error.rs`, add to the enum:

```rust
#[error("synapse error: {0}")]
Synapse(String),
```

**Step 4: Run test to verify it passes**

Run: `cargo test error::tests::error_display_synapse`
Expected: PASS

**Step 5: Create `src/synapse.rs` with types and stub**

```rust
use serde::{Deserialize, Serialize};

use crate::error::{LegionError, Result};

/// Result of a VALIDATE call.
#[derive(Debug, Clone, PartialEq)]
pub enum ValidationResult {
    Accept,
    Reject { reason: String },
}

/// Result of a CLASSIFY call.
#[derive(Debug, Clone)]
pub struct Classification {
    pub domain: Option<String>,
    pub tags: Vec<String>,
    pub transfer: bool,
    pub specificity: f32,
}

/// Anthropic API message types.
#[derive(Serialize)]
struct ApiRequest {
    model: String,
    max_tokens: u32,
    messages: Vec<ApiMessage>,
}

#[derive(Serialize)]
struct ApiMessage {
    role: String,
    content: String,
}

#[derive(Deserialize)]
struct ApiResponse {
    content: Vec<ApiContentBlock>,
}

#[derive(Deserialize)]
struct ApiContentBlock {
    text: Option<String>,
}

const SONNET_MODEL: &str = "claude-sonnet-4-20250514";
const API_URL: &str = "https://api.anthropic.com/v1/messages";

/// Read the ANTHROPIC_API_KEY from the environment.
fn api_key() -> Result<String> {
    std::env::var("ANTHROPIC_API_KEY").map_err(|_| {
        LegionError::Synapse("ANTHROPIC_API_KEY not set".to_string())
    })
}
```

**Step 6: Register the module in `main.rs`**

Add `mod synapse;` to the module declarations.

**Step 7: Run clippy and tests**

Run: `cargo clippy -- -D warnings && cargo test`
Expected: PASS

**Step 8: Commit**

```bash
git add src/error.rs src/synapse.rs src/main.rs
git commit -m "feat: add Synapse error variant and API types"
```

---

### Task 3: Implement VALIDATE primitive

**Files:**
- Modify: `src/synapse.rs`

**Step 1: Write the failing test for validate**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_validate_accept() {
        let response = "ACCEPT";
        let result = parse_validate_response(response);
        assert_eq!(result, ValidationResult::Accept);
    }

    #[test]
    fn parse_validate_reject() {
        let response = "REJECT: too vague, lacks actionable detail";
        let result = parse_validate_response(response);
        match result {
            ValidationResult::Reject { reason } => {
                assert!(reason.contains("too vague"));
            }
            _ => panic!("expected Reject"),
        }
    }

    #[test]
    fn parse_validate_malformed_defaults_accept() {
        // Fail-open: if we can't parse, accept
        let result = parse_validate_response("gibberish response");
        assert_eq!(result, ValidationResult::Accept);
    }
}
```

**Step 2: Run tests to verify they fail**

Run: `cargo test synapse::tests`
Expected: FAIL - `parse_validate_response` not found

**Step 3: Implement parse_validate_response**

```rust
/// Parse the LLM response for a VALIDATE call.
///
/// Expected format: "ACCEPT" or "REJECT: reason"
/// Defaults to Accept (fail-open) if the response is unparseable.
fn parse_validate_response(response: &str) -> ValidationResult {
    let trimmed = response.trim();
    if trimmed.starts_with("REJECT") {
        let reason = trimmed
            .strip_prefix("REJECT:")
            .or_else(|| trimmed.strip_prefix("REJECT"))
            .unwrap_or("")
            .trim()
            .to_string();
        ValidationResult::Reject {
            reason: if reason.is_empty() {
                "no reason given".to_string()
            } else {
                reason
            },
        }
    } else {
        ValidationResult::Accept
    }
}
```

**Step 4: Run tests to verify they pass**

Run: `cargo test synapse::tests`
Expected: PASS

**Step 5: Implement the validate API call**

```rust
/// Validate a reflection's quality via LLM.
///
/// Calls Sonnet with the candidate text and quality criteria.
/// Returns Accept or Reject with reason.
/// Fails open: API errors return Accept with a warning to stderr.
pub fn validate(text: &str, similar_texts: &[String]) -> Result<ValidationResult> {
    let key = match api_key() {
        Ok(k) => k,
        Err(e) => {
            eprintln!("[synapse] validation skipped: {e}");
            return Ok(ValidationResult::Accept);
        }
    };

    let similar_context = if similar_texts.is_empty() {
        String::from("No existing reflections to compare against.")
    } else {
        let items: Vec<String> = similar_texts
            .iter()
            .enumerate()
            .map(|(i, t)| format!("{}. {}", i + 1, t))
            .collect();
        format!("Existing similar reflections:\n{}", items.join("\n"))
    };

    let prompt = format!(
        "You are a quality gate for an agent memory system. \
         Evaluate whether this reflection is worth storing.\n\n\
         Candidate reflection:\n\"{}\"\n\n\
         {}\n\n\
         Criteria:\n\
         - Is it specific and actionable (not vague platitudes)?\n\
         - Does it add new knowledge (not duplicate existing reflections)?\n\
         - Would another agent find this useful in the future?\n\n\
         Respond with exactly one line:\n\
         ACCEPT\n\
         or\n\
         REJECT: <brief reason>",
        text, similar_context
    );

    match call_api(&key, &prompt) {
        Ok(response) => Ok(parse_validate_response(&response)),
        Err(e) => {
            eprintln!("[synapse] validation failed, accepting anyway: {e}");
            Ok(ValidationResult::Accept)
        }
    }
}
```

**Step 6: Implement call_api helper**

```rust
/// Call the Anthropic Messages API with a single user message.
fn call_api(api_key: &str, prompt: &str) -> Result<String> {
    let rt = tokio::runtime::Runtime::new().map_err(|e| {
        LegionError::Synapse(format!("failed to create async runtime: {e}"))
    })?;

    rt.block_on(async {
        let client = reqwest::Client::new();

        let request = ApiRequest {
            model: SONNET_MODEL.to_string(),
            max_tokens: 256,
            messages: vec![ApiMessage {
                role: "user".to_string(),
                content: prompt.to_string(),
            }],
        };

        let response = client
            .post(API_URL)
            .header("x-api-key", api_key)
            .header("anthropic-version", "2023-06-01")
            .header("content-type", "application/json")
            .json(&request)
            .send()
            .await
            .map_err(|e| LegionError::Synapse(format!("API request failed: {e}")))?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            return Err(LegionError::Synapse(format!(
                "API returned {status}: {body}"
            )));
        }

        let api_response: ApiResponse = response
            .json()
            .await
            .map_err(|e| LegionError::Synapse(format!("failed to parse API response: {e}")))?;

        api_response
            .content
            .first()
            .and_then(|block| block.text.clone())
            .ok_or_else(|| LegionError::Synapse("empty API response".to_string()))
    })
}
```

**Step 7: Run clippy and tests**

Run: `cargo clippy -- -D warnings && cargo test synapse::tests`
Expected: PASS (unit tests don't call the API)

**Step 8: Commit**

```bash
git add src/synapse.rs
git commit -m "feat: implement VALIDATE primitive with fail-open semantics"
```

---

### Task 4: Implement CLASSIFY primitive

**Files:**
- Modify: `src/synapse.rs`

**Step 1: Write the failing tests for classify parsing**

```rust
#[test]
fn parse_classify_full() {
    let response = r#"{"domain":"color-tokens","tags":["semantic","consumer"],"transfer":true,"specificity":0.8}"#;
    let result = parse_classify_response(response);
    assert_eq!(result.domain.as_deref(), Some("color-tokens"));
    assert_eq!(result.tags, vec!["semantic", "consumer"]);
    assert!(result.transfer);
    assert!((result.specificity - 0.8).abs() < f32::EPSILON);
}

#[test]
fn parse_classify_minimal() {
    let response = r#"{"domain":null,"tags":[],"transfer":false,"specificity":0.5}"#;
    let result = parse_classify_response(response);
    assert!(result.domain.is_none());
    assert!(result.tags.is_empty());
    assert!(!result.transfer);
}

#[test]
fn parse_classify_malformed_returns_defaults() {
    let result = parse_classify_response("not json at all");
    assert!(result.domain.is_none());
    assert!(result.tags.is_empty());
    assert!(!result.transfer);
    assert!((result.specificity - 0.5).abs() < f32::EPSILON);
}
```

**Step 2: Run tests to verify they fail**

Run: `cargo test synapse::tests::parse_classify`
Expected: FAIL

**Step 3: Implement parse_classify_response**

```rust
/// JSON structure expected from the CLASSIFY LLM response.
#[derive(Deserialize)]
struct ClassifyJson {
    domain: Option<String>,
    #[serde(default)]
    tags: Vec<String>,
    #[serde(default)]
    transfer: bool,
    #[serde(default = "default_specificity")]
    specificity: f32,
}

fn default_specificity() -> f32 {
    0.5
}

/// Parse the LLM response for a CLASSIFY call.
///
/// Expected format: JSON object with domain, tags, transfer, specificity.
/// Returns sensible defaults if parsing fails.
fn parse_classify_response(response: &str) -> Classification {
    // Try to extract JSON from the response (LLM may wrap it in text)
    let json_str = extract_json_object(response);

    match serde_json::from_str::<ClassifyJson>(&json_str) {
        Ok(parsed) => Classification {
            domain: parsed.domain,
            tags: parsed.tags,
            transfer: parsed.transfer,
            specificity: parsed.specificity.clamp(0.0, 1.0),
        },
        Err(_) => Classification {
            domain: None,
            tags: vec![],
            transfer: false,
            specificity: 0.5,
        },
    }
}

/// Extract a JSON object from a string that may contain surrounding text.
fn extract_json_object(s: &str) -> String {
    let trimmed = s.trim();
    if let Some(start) = trimmed.find('{') {
        if let Some(end) = trimmed.rfind('}') {
            return trimmed[start..=end].to_string();
        }
    }
    trimmed.to_string()
}
```

**Step 4: Run tests to verify they pass**

Run: `cargo test synapse::tests::parse_classify`
Expected: PASS

**Step 5: Implement the classify API call**

```rust
/// Classify a reflection via LLM.
///
/// Calls Sonnet to determine domain, tags, transfer potential, and specificity.
/// Fails open: API errors return default classification.
pub fn classify(text: &str) -> Result<Classification> {
    let key = match api_key() {
        Ok(k) => k,
        Err(e) => {
            eprintln!("[synapse] classification skipped: {e}");
            return Ok(Classification {
                domain: None,
                tags: vec![],
                transfer: false,
                specificity: 0.5,
            });
        }
    };

    let prompt = format!(
        "You are a classifier for an agent memory system. \
         Analyze this reflection and return a JSON object.\n\n\
         Reflection:\n\"{}\"\n\n\
         Return exactly one JSON object with these fields:\n\
         - \"domain\": string or null (short topic slug like \"color-tokens\", \"auth\", \"testing\")\n\
         - \"tags\": array of strings (relevant keywords, 1-5 tags)\n\
         - \"transfer\": boolean (true if this insight could help agents working in other domains)\n\
         - \"specificity\": float 0.0-1.0 (0.0 = vague platitude, 1.0 = precise actionable detail)\n\n\
         Respond with ONLY the JSON object, no other text.",
        text
    );

    match call_api(&key, &prompt) {
        Ok(response) => Ok(parse_classify_response(&response)),
        Err(e) => {
            eprintln!("[synapse] classification failed, using defaults: {e}");
            Ok(Classification {
                domain: None,
                tags: vec![],
                transfer: false,
                specificity: 0.5,
            })
        }
    }
}
```

**Step 6: Run clippy and tests**

Run: `cargo clippy -- -D warnings && cargo test synapse::tests`
Expected: PASS

**Step 7: Commit**

```bash
git add src/synapse.rs
git commit -m "feat: implement CLASSIFY primitive with JSON parsing"
```

---

### Task 5: Wire --synapse flag into Reflect command

**Files:**
- Modify: `src/main.rs`

**Step 1: Add --synapse flag to Reflect**

In the `Commands::Reflect` variant, add:

```rust
/// Enable Synapse quality gate (requires ANTHROPIC_API_KEY)
#[arg(long)]
synapse: bool,
```

**Step 2: Wire synapse into reflect handler**

After the reflection is stored (post `run_compound_command_with_meta`), before embedding backfill, add:

```rust
if synapse {
    // Get the 3 most similar existing reflections for context
    let similar = match try_load_embed_model() {
        Some(ref model) => {
            let index = search::SearchIndex::open(&base.join("index"))?;
            get_similar_texts(&database, &index, model, repo.first().map(|s| s.as_str()), text.as_deref().unwrap_or(""), 3)?
        }
        None => vec![],
    };

    match synapse::validate(text.as_deref().unwrap_or(""), &similar) {
        Ok(synapse::ValidationResult::Accept) => {
            eprintln!("[synapse] reflection accepted");
        }
        Ok(synapse::ValidationResult::Reject { reason }) => {
            eprintln!("[synapse] reflection rejected: {}", reason);
            // Note: reflection is already stored. We log rejection but
            // don't delete -- the agent can decide what to do.
        }
        Err(e) => {
            eprintln!("[synapse] validation error (fail-open): {}", e);
        }
    }

    match synapse::classify(text.as_deref().unwrap_or("")) {
        Ok(classification) => {
            // Update the reflection with classification metadata
            // if the reflection was stored without domain/tags
            if let Some(first_repo) = repo.first() {
                apply_classification(&database, first_repo, &classification)?;
            }
            eprintln!("[synapse] classified: domain={:?} tags={:?} transfer={} specificity={:.1}",
                classification.domain, classification.tags, classification.transfer, classification.specificity);
        }
        Err(e) => {
            eprintln!("[synapse] classification error (fail-open): {}", e);
        }
    }
}
```

Actually, this approach is wrong -- we need to validate BEFORE storing, and classify can happen after. Let me redesign the flow:

**Revised approach:** The `--synapse` flag should:
1. VALIDATE before storing (if rejected, print reason and exit without storing)
2. CLASSIFY after storing (updates metadata on the stored reflection)

This means we need to restructure the reflect flow when synapse is enabled. Instead of calling `run_compound_command_with_meta` directly, we validate first, then store.

Let me revise this task.

**Step 2 (revised): Wire synapse into reflect handler**

In the `Commands::Reflect` match arm, restructure:

```rust
Commands::Reflect {
    repo,
    text,
    transcript,
    domain,
    tags,
    follows,
    synapse,
} => {
    let base = data_dir()?;
    let database = db::Database::open(&base.join("legion.db"))?;
    let index = search::SearchIndex::open(&base.join("index"))?;

    // Resolve the text content (for synapse validation)
    let resolved_text = match (&text, &transcript) {
        (Some(t), None) => Some(t.clone()),
        (None, Some(path)) => Some(reflect::extract_last_assistant_message(path)?),
        (Some(_), Some(_)) => return Err(error::LegionError::NoReflectionInput),
        (None, None) => return Err(error::LegionError::NoReflectionInput),
    };

    // Synapse validation gate (before storing)
    if synapse {
        if let Some(ref content) = resolved_text {
            let similar = get_similar_for_synapse(
                &database, &index, repo.first().map(|s| s.as_str()), content
            );
            match synapse::validate(content, &similar) {
                Ok(synapse::ValidationResult::Accept) => {
                    eprintln!("[synapse] reflection accepted");
                }
                Ok(synapse::ValidationResult::Reject { reason }) => {
                    eprintln!("[synapse] reflection rejected: {}", reason);
                    return Ok(());
                }
                Err(e) => {
                    eprintln!("[synapse] validation error (fail-open): {}", e);
                }
            }
        }
    }

    let meta = db::ReflectionMeta { domain, tags, parent_id: follows };

    run_compound_command_with_meta(
        &database, &index, &repo, &text, &transcript, &meta,
        reflect::reflect_from_text_with_meta,
        reflect::reflect_from_transcript_with_meta,
        "storing reflection",
    )?;

    // Synapse classification (after storing)
    if synapse {
        if let Some(ref content) = resolved_text {
            if let Ok(classification) = synapse::classify(content) {
                // Update the most recently stored reflections with classification
                for r in &repo {
                    apply_synapse_classification(&database, r, &classification)?;
                }
                eprintln!(
                    "[synapse] classified: domain={:?} tags={:?} transfer={} specificity={:.1}",
                    classification.domain, classification.tags,
                    classification.transfer, classification.specificity
                );
            }
        }
    }

    // Compute embeddings for new reflections
    if let Some(model) = try_load_embed_model() {
        let n = backfill_embeddings(&database, &model)?;
        if n > 0 {
            eprintln!("[legion] embedded {} reflections", n);
        }
    }
}
```

**Step 3: Add helper functions**

```rust
/// Get similar reflection texts for Synapse validation context.
fn get_similar_for_synapse(
    db: &db::Database,
    index: &search::SearchIndex,
    repo: Option<&str>,
    text: &str,
) -> Vec<String> {
    let search_fn = match repo {
        Some(r) => index.search(r, text, 3),
        None => index.search_all(text, 3),
    };
    match search_fn {
        Ok(results) => results
            .iter()
            .filter_map(|sr| {
                db.get_reflection_by_id(&sr.id)
                    .ok()
                    .flatten()
                    .map(|r| r.text)
            })
            .collect(),
        Err(_) => vec![],
    }
}

/// Apply Synapse classification to the most recent reflection for a repo.
fn apply_synapse_classification(
    db: &db::Database,
    repo: &str,
    classification: &synapse::Classification,
) -> error::Result<()> {
    let latest = db.get_latest_reflections(repo, 1)?;
    if let Some(reflection) = latest.first() {
        let domain = classification.domain.as_deref();
        let tags = if classification.tags.is_empty() {
            None
        } else {
            Some(classification.tags.join(","))
        };
        db.update_classification(&reflection.id, domain, tags.as_deref())?;
    }
    Ok(())
}
```

**Step 4: Add `update_classification` to Database**

In `src/db.rs`:

```rust
/// Update a reflection's domain and tags from Synapse classification.
///
/// Only overwrites if the current values are None (manual metadata takes priority).
pub fn update_classification(
    &self,
    id: &str,
    domain: Option<&str>,
    tags: Option<&str>,
) -> Result<()> {
    self.conn.execute(
        "UPDATE reflections SET \
         domain = COALESCE(domain, ?1), \
         tags = COALESCE(tags, ?2) \
         WHERE id = ?3",
        rusqlite::params![domain, tags, id],
    )?;
    Ok(())
}
```

**Step 5: Write test for update_classification**

In `src/db.rs` tests:

```rust
#[test]
fn update_classification_fills_empty_fields() {
    let db = test_db();
    let r = db.insert_reflection("kelex", "test", "self").unwrap();
    assert!(r.domain.is_none());
    assert!(r.tags.is_none());

    db.update_classification(&r.id, Some("auth"), Some("security,tokens")).unwrap();

    let updated = db.get_reflection_by_id(&r.id).unwrap().unwrap();
    assert_eq!(updated.domain.as_deref(), Some("auth"));
    assert_eq!(updated.tags.as_deref(), Some("security,tokens"));
}

#[test]
fn update_classification_preserves_manual_metadata() {
    let db = test_db();
    let meta = ReflectionMeta {
        domain: Some("color-tokens".into()),
        tags: Some("manual-tag".into()),
        parent_id: None,
    };
    let r = db.insert_reflection_with_meta("kelex", "test", "self", &meta).unwrap();

    db.update_classification(&r.id, Some("wrong-domain"), Some("wrong-tag")).unwrap();

    let updated = db.get_reflection_by_id(&r.id).unwrap().unwrap();
    assert_eq!(updated.domain.as_deref(), Some("color-tokens"));
    assert_eq!(updated.tags.as_deref(), Some("manual-tag"));
}
```

**Step 6: Run all tests**

Run: `cargo test`
Expected: PASS

**Step 7: Commit**

```bash
git add src/main.rs src/db.rs
git commit -m "feat: wire --synapse flag into Reflect command with validate-then-classify flow"
```

---

### Task 6: Wire --synapse flag into Post command

**Files:**
- Modify: `src/main.rs`

**Step 1: Add --synapse flag to Post**

In the `Commands::Post` variant, add:

```rust
/// Enable Synapse classification (validation is skipped for board posts)
#[arg(long)]
synapse: bool,
```

**Step 2: Wire synapse classify into post handler (no validate)**

Board posts bypass validation (they're intentional communication). But they still get classified.

After `run_compound_command_with_meta` in the Post handler:

```rust
// Synapse classification only (board posts bypass validation)
if synapse {
    let content = match (&text, &transcript) {
        (Some(t), _) => Some(t.clone()),
        (_, Some(path)) => reflect::extract_last_assistant_message(path).ok(),
        _ => None,
    };
    if let Some(ref content) = content {
        if let Ok(classification) = synapse::classify(content) {
            for r in &repo {
                apply_synapse_classification(&database, r, &classification)?;
            }
            eprintln!(
                "[synapse] classified: domain={:?} tags={:?}",
                classification.domain, classification.tags
            );
        }
    }
}
```

**Step 3: Run clippy and tests**

Run: `cargo clippy -- -D warnings && cargo test`
Expected: PASS

**Step 4: Commit**

```bash
git add src/main.rs
git commit -m "feat: wire --synapse flag into Post command (classify only, no validation)"
```

---

### Task 7: Add standalone Synapse command for debugging

**Files:**
- Modify: `src/main.rs`

**Step 1: Add Synapse subcommand**

```rust
/// Run Synapse quality gate or classification directly
Synapse {
    /// Action to perform: "validate" or "classify"
    #[arg(long)]
    action: String,

    /// Text to validate or classify
    #[arg(long)]
    text: String,

    /// Repository context for finding similar reflections (validate only)
    #[arg(long)]
    repo: Option<String>,
},
```

**Step 2: Wire the handler**

```rust
Commands::Synapse { action, text, repo } => {
    match action.as_str() {
        "validate" => {
            let base = data_dir()?;
            let database = db::Database::open(&base.join("legion.db"))?;
            let index = search::SearchIndex::open(&base.join("index"))?;
            let similar = get_similar_for_synapse(
                &database, &index, repo.as_deref(), &text,
            );
            let result = synapse::validate(&text, &similar)?;
            match result {
                synapse::ValidationResult::Accept => println!("ACCEPT"),
                synapse::ValidationResult::Reject { reason } => {
                    println!("REJECT: {}", reason)
                }
            }
        }
        "classify" => {
            let result = synapse::classify(&text)?;
            println!(
                "domain: {:?}\ntags: {:?}\ntransfer: {}\nspecificity: {:.2}",
                result.domain, result.tags, result.transfer, result.specificity
            );
        }
        other => {
            eprintln!("[synapse] unknown action: {} (use 'validate' or 'classify')", other);
            return Err(error::LegionError::Synapse(
                format!("unknown action: {other}")
            ));
        }
    }
}
```

**Step 3: Run clippy**

Run: `cargo clippy -- -D warnings`
Expected: PASS

**Step 4: Commit**

```bash
git add src/main.rs
git commit -m "feat: add standalone Synapse command for debugging/testing"
```

---

### Task 8: Integration test for synapse flow

**Files:**
- Modify: `tests/integration.rs`

**Step 1: Write integration tests**

These test the CLI behavior without an API key (fail-open path):

```rust
#[test]
fn synapse_flag_without_api_key_stores_anyway() {
    // Without ANTHROPIC_API_KEY, --synapse should fail-open
    let dir = tempdir().expect("tempdir");
    let cmd = legion_cmd(&dir);
    let output = cmd
        .arg("reflect")
        .arg("--repo").arg("test")
        .arg("--text").arg("test reflection with synapse")
        .arg("--synapse")
        .output()
        .expect("run");

    // Should succeed (fail-open)
    assert!(output.status.success(), "stderr: {}", String::from_utf8_lossy(&output.stderr));

    // Verify the reflection was stored
    let recall_output = legion_cmd(&dir)
        .arg("recall")
        .arg("--repo").arg("test")
        .arg("--context").arg("test reflection")
        .output()
        .expect("recall");
    let stdout = String::from_utf8_lossy(&recall_output.stdout);
    assert!(stdout.contains("test reflection with synapse"));
}

#[test]
fn synapse_standalone_validate_without_api_key() {
    let dir = tempdir().expect("tempdir");
    let output = legion_cmd(&dir)
        .arg("synapse")
        .arg("--action").arg("validate")
        .arg("--text").arg("test reflection")
        .output()
        .expect("run");

    // Should print ACCEPT (fail-open when no API key)
    // Actually this should error since standalone doesn't fail-open the same way
    // The standalone command surfaces the error directly
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("ANTHROPIC_API_KEY") || output.status.success());
}
```

**Step 2: Run integration tests**

Run: `cargo test --test integration`
Expected: PASS

**Step 3: Commit**

```bash
git add tests/integration.rs
git commit -m "test: add integration tests for synapse fail-open behavior"
```

---

### Task 9: Update CLAUDE.md and documentation

**Files:**
- Modify: `CLAUDE.md`

**Step 1: Update commands section**

Add to the commands block:

```bash
# With Synapse quality gate
legion reflect --repo <name> --text "..." --synapse
legion synapse --action validate --text "candidate text"
legion synapse --action classify --text "candidate text"
```

**Step 2: Update Phase Plan**

Change Phase 3.0 from future to complete:

```
4. **Phase 3.0** (complete): Synapse quality gate. LLM classification via Anthropic API (Sonnet). --synapse flag on reflect/post. Two primitives: VALIDATE (accept/reject) and CLASSIFY (auto-tag domain, tags, transfer, specificity). Fail-open on API errors.
```

**Step 3: Commit**

```bash
git add CLAUDE.md
git commit -m "docs: update CLAUDE.md with Phase 3.0 Synapse commands and status"
```

---

### Task 10: Final validation and build

**Step 1: Run full test suite**

Run: `cargo test`
Expected: ALL PASS

**Step 2: Run clippy**

Run: `cargo clippy -- -D warnings`
Expected: PASS

**Step 3: Run fmt check**

Run: `cargo fmt -- --check`
Expected: PASS

**Step 4: Install the binary**

Run: `cargo install --path .`
Expected: binary updated at ~/.cargo/bin/legion

**Step 5: Smoke test**

Run: `legion synapse --action classify --text "test"`
Expected: either classification output (if API key set) or error about ANTHROPIC_API_KEY

**Step 6: Commit any final fixes**

```bash
git add -A
git commit -m "chore: final validation for Phase 3.0 Synapse"
```

---

## Dependency Graph

```
Task 1 (reqwest dep)
  -> Task 2 (error + types)
    -> Task 3 (VALIDATE)
    -> Task 4 (CLASSIFY)
      -> Task 5 (wire Reflect)
      -> Task 6 (wire Post)
      -> Task 7 (standalone cmd)
        -> Task 8 (integration tests)
          -> Task 9 (docs)
            -> Task 10 (final validation)
```

Tasks 3 and 4 can run in parallel after Task 2.
Tasks 5, 6, and 7 can run in parallel after Tasks 3+4.
