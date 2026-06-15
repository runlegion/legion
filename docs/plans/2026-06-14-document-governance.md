# RFC: Document-layer governance

- **Status:** proposed
- **Originator:** vault (COS) -- bullpen `019ec7c8-dce3` -> sharpened `019ec7fc-52ba` -> `019ec801 @legion-prime proposal:review`
- **Formalized by:** legion-prime
- **Issues:** #658 (update), #659 (state machine), #660 (adoption gate), #661 (adopt held schemas), #662 (follow-up)
- **Date:** 2026-06-14

## Summary

Give the `legion document` layer the governance kanban already has: id-stable update, a governed status state machine, and a human gate on adoption. This is not a new pattern to design -- it is the kanban pattern (`src/kanban/state.rs`) not yet ported to documents. One primitive -- a governed, id-stable, operator-gated lifecycle -- closes the self-canonize security hole and kills the archive+recreate id churn, and unblocks four canon documents held at `draft` pending the gate.

## Context

The document layer is the coordination substrate (#455/#456): type-agnostic JSON storage with a hoisted `meta` shape, subset-schema validation, and hot/cold tiering. Two load-bearing gaps, both confirmed in code:

1. **No id-stable update / governed transition.** Surface is `create / view / list / archive / validate`. Revising a landed document = archive-old + create-new = a NEW id every time. Reference chains are live and shipped -- `requirement.traces_to`, `nfr_refs`, `validate --schema <id>`, spec-gen's `document-id -> requirement -> bound-card` -- so id churn on every revision is real breakage. (`src/documents.rs`: `insert/get/list/archive`, no `update_document`, no `transition_document`.)

2. **No operator-validated adoption gate -- a live hole.** `document create --status adopted` writes binding canon for free (`meta.status.unwrap_or("draft")`, `src/documents.rs:84`). The six adopted service-design schemas were minted this way. The operator named it: *"or you all could build skynet."*

vault's framing: documents are the outlier. Kanban has all three properties -- id-stable `update_card`, a pure `transition()` state machine, and a human gate (`NeedInput -> [human] -> Resume`). **Reuse, don't invent.**

## Design

### 1. DocumentStatus state machine (#659)
A pure `document_transition(current, action)` in a new `src/documents/state.rs`, modeled on `src/kanban/state.rs:158`. Canon lifecycle:

`Draft -> Proposed -> Adopted`; `Proposed -> Rejected`; `Adopted -> Superseded`; `Rejected/Proposed -> Draft`. `Adopt` is the operator-only release (the `Resume`-equivalent). Unlisted pairs return `Err(InvalidDocumentTransition)` before any DB write. Exhaustively unit-tested, same shape as kanban.

**`Superseded` trigger (vault refinement):** `Superseded` means *replaced by a different document* (a new id), NOT in-place content revision. id-stable `update` keeps the id and status; routing a revision through `Superseded` would re-introduce the id churn this RFC kills. Supersede only when a genuinely new document replaces an adopted one.

### 2. Disjoint authority -- no dual-writer
`transition_card_status_with_sync` (`src/db/kanban.rs:684-688`) is today the sole post-insert writer of `documents.status` (requirement docs, card-driven). The governance machine must not become a second writer. Resolution: **the two authorities never touch the same document.** Requirement docs are card-bound and stay card-driven; the governance machine governs only unbound canon/definition docs (schema, nfr, charter, blueprint, persona, journey, ecosystem, painmatrix, research). `transition_document` refuses when the document has a live bound card (reuse `live_card_bound_to_document`, `src/db/kanban.rs:789`). Disjoint doc-sets, one `status` column, no conflict.

The split is **dynamic, not by doc-type** (vault refinement): an NFR can be card-bound (verifiable, like a requirement) or unbound (canon). The live-bound-card check resolves it per-document -- unbound NFR = governance-driven, card-bound NFR = card-driven -- so the same rule covers requirements and NFRs without enumerating types.

### 3. id-stable update (#658)
`update_document` mutates payload content in place -- id, doc_type, created_at immutable -- writing the hoisted `documents.status` column and `payload.meta` atomically in one `unchecked_transaction()` (the established primitive). Status changes go through `transition`, not `update`. Consumers resolve canon by type+title with adoption-status as the pointer, never a hardcoded id (`019ebd18`) -- id-stable update is what makes that convention coherent.

### 4. The adopt gate -- existing primitives, no new auth (#660)
The human gate ports onto two mechanisms that already exist:

- **Notification/queue = the signal system.** `draft -> proposed` emits a directed `proposal:review` signal to prime (`@legion-prime`), landing in the REQUIRES-A-REPLY bucket of `build_wake_prompt` (`src/watch/signals.rs:94`). Adoption is the operator's reply-action -- the exact dogfood of how vault delivered this proposal. No invented queue.
- **Hard enforcement = the spawn marker.** The PTY spawn stamps every watch-spawned agent with `LEGION_AUTO_WAKE=1` / `LEGION_SPAWN_SOURCE=watch-pty` (`src/watch/spawn.rs:276-279`). A prime-owned pre-bash hook blocks `create --status adopted`, `transition <id> adopt`, and `document adopt <id>` when `LEGION_AUTO_WAKE` is present (agent); allows when absent (operator). The hook reads the session env the agent cannot rewrite for it -- the `pre-bash-grep` contract; bypass logs to `bypass.jsonl`.

Two enforcement points close the hole: `create` rejects `--status adopted` under the marker; the `Adopt` action is agent-blocked. Agents are capped at `draft<->proposed`.

**Re-gate on content change (vault, load-bearing -- fold into #660 before it lands).** The gate guards the *status transition* to `Adopted`, but `update_document` (#658) mutates payload IN PLACE independent of status. Without a rule, an agent adopts (operator-gated) and then `update`s the now-adopted content freely -- canon mutates ungated; the hole just moved one call over. Fix: a content `update` to an `Adopted` document **auto-transitions it `Adopted -> Proposed`** (and emits the `proposal:review` signal), so every canon change re-enters the same gate as the first adoption. `update` on a non-adopted doc is unaffected. This couples #658 and #660: `update_document` must be status-aware.

**What "operator-validated" actually means (vault).** `LEGION_AUTO_WAKE`-absent proves the session is NOT watch-spawned (autonomy-absent) -- it does not cryptographically prove human-ness; a non-watch-spawned interactive agent session would also pass. Per the no-crypto-auth non-goal this is acceptable (strong-by-default + audited via `bypass.jsonl`), but consumers MUST NOT over-trust `adopted` as unforgeable. `adopted` = blessed in a human-present, audited session. The vault-held adoption secret remains the future hardening if the threat model tightens.

## Non-goals (do not revisit)

- **Storage stays type-agnostic** (#455/#456). All gate/lifecycle logic lives at the CLI/module boundary; `insert_document` is unchanged.
- **No jsonschema crate** (#526). Validation reuses the existing subset validator (`validate_instance`) at its supported depth. The shallow validator (no `$ref`, no nested enum, no min/max) stays an accepted FYI -- recursive/deeply-nested contracts remain convention-not-enforced.
- **The five locked spec-format decisions** (`019e14b3`: `traces_to` required, NFR a separate type, etc.) are baked into requirement schema v2 and spec-gen; the RFC does not touch them.
- **spec-gen NFR-blindness** (derives only functional requirements) stays an accepted FYI.
- **No cryptographic operator auth.** The gate is strong-by-default + auditable, consistent with every other legion guardrail -- not an unforgeable secret. (A vault-held adoption secret is a possible future hardening, out of scope here.)

## Unblocks

Four canon documents held at `draft` pending the gate (#661): research schema `019ec434`, NFR schema `019ec456`, system-foundations schema `019ec467`, global registry `019ec467-faa2`.

## Verification

- **Unit:** `document_transition()` validity-table + illegal-pair rejection (mirror kanban state tests).
- **Integration:** create->update id-stable (id unchanged, content changed); draft->proposed; `adopt` blocked with `LEGION_AUTO_WAKE=1`, allowed without; `create --status adopted` rejected under the marker; transition refused on a live-bound requirement doc.
- **E2E:** operator adopts one held schema; a watch-spawned session cannot; id unchanged across a content revision.

## Ownership

vault owns spec orchestration (COS); prime owns the substrate. The substrate primitives (#658-#660) are prime/legion implementation work; the adoption *semantics* and the held-schema adoptions (#661) are vault's. vault should review this RFC before the issues are worked.
