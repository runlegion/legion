---
name: sd-discover
description: |
  The evidence step of service design: take the research agenda's claims to test, query
  eavesdrop's discourse corpus, apply the 0.40 disconfirm rule, surface emergent insights the
  intent missed, and land one schema-valid Discovery document. Parks on a missing corpus
  rather than treating no evidence as a verdict. Invoke after sd-intent-review.
version: 0.1.0
user-invocable: true
allowed-tools: Bash, Read
---

# Discover: claims in, Discovery out

This is the step that talks to the world. Its authority comes from being willing to lose:
a claim the intent asserted gets CONTRADICTED here when the discourse does not support it, and it
stays contradicted downstream. It is also the step most likely to park, because corpora take
hours to build.

## Probe the corpus first

Before scoring anything, probe every `evidence_target` in the agenda with a cheap query
through eavesdrop's CLI (the closed query surface; its usage guide lives in eavesdrop's
memory -- `legion consult --context "querying eavesdrop agent CLI guide"` if you need it).

**No evidence is not a verdict.** A query returning nothing for every target means the
corpus is missing or thin, not that the pain is disconfirmed. The distinction is the whole
point of this section. On a missing or thin corpus:

1. Start the crawl. The real sequence is three steps, not one: `eavesdrop init <lens>`
   creates the named config; write the agenda's named targets into it by hand
   (`~/Library/Application Support/eavesdrop/<lens>.toml` -- subreddits, feed URLs); then
   `eavesdrop crawl <lens>` crawls the named config. `eavesdrop discover` is a dead end
   here: Reddit discovery wants REDDIT_CLIENT_ID and crawling itself needs no credentials
   (Arctic Shift) -- do not go looking for any.
2. Give the crawl an owner: `legion signal` to the eavesdrop agent naming the lens and
   why, so its completion (or failure) comes back as a wake. Ask it to keep the lens warm
   (`eavesdrop daemon <lens> -i 6h`) -- discovery at real depth takes about a day of
   accumulation, not one pass.
3. Park per the protocol in the sd-service-design skill: land the Discovery as a `draft`
   with the provable themes scored and the blocked ones named
   (`blocked: crawl <lens> in flight`), store the anchor reflection, and arm
   `legion defer --work-item sd-<repo>-discover --repo <repo> --until 1d` alongside the
   signal. A blocked theme carries `status: "blocked"` with placeholder zero scores, and
   the park state and its resume query in `evidence.gaps` and `evidence.next_probe`.

**The two-pass cadence.** The first crawl slice supports an ORIENTATION pass only: run the
probes, score what genuinely can be scored, record per-theme what came back, and RE-ARM the
defer -- never clear it. The authoritative scoring pass runs on the wake, over the corpus a
day of re-crawl has built. Only the authoritative pass clears the defer
(`legion undefer --work-item <id>` -- it takes no `--repo`). A completed crawl whose slice
turns out unusable (spam-dominated, off-topic) is a third park state, distinct from
missing-corpus: name it `blocked on source depth`, keep the insights blocked, and escalate
the source-mix decision to the operator -- more crawl time will not fix the wrong net.

## Score what the corpus can answer

For each claim with evidence available:

- Query the corpus for the claim's target; read what people actually say, not what the
  intent hoped they would say.
- **The 0.40 disconfirm rule:** an on-topic relevance score under 0.40 for a
  intent-asserted claim is CONTRADICTED. State it so, in the Discovery, with the score --
  never soften a contradiction into "weak support."
- Score surviving claims into insights on the schema's five axes -- frequency, intensity,
  friction, urgency, fit -- each 0 to 5, plus the weighted composite; the document's
  top-level `weights` object carries the axis weights (0 to 1) used for that composite,
  and `meta.threshold` carries the disconfirm bar (0.40). Each insight carries its
  `status` verdict -- supported, bounded, contradicted, blocked, or saturated-unevidenced
  -- with `emergent` as orthogonal provenance -- and its `workaround`:
  what people do about this pain today, which is half the evidence the pain is real.
  Quantify the pain's `evidence.cost` only when the evidence supports a number; never
  invent one. Evidence citations are
  structural, not prose: each theme's `evidence.eavesdrop` array carries
  `{source, url, score, text}` rows with the speakers' own words in `text`.
- **Emergent insights:** discourse that keeps returning to something the intent never
  claimed is a finding, not noise. Add it as an insight, `emergent: true`, with the same
  evidence bar and any verdict it earns.

## The inverse pass (required)

The pass runs once, at the AUTHORITATIVE scoring pass, after first scoring and before
the final land. An orientation draft parks without it -- its supported insights are
provisional until the wake, and the counter-probes would be spent against a slice.
At the authoritative pass, attack your own supported insights. An all-supported
Discovery is unfalsified, not confirmed: probes written by a theme's author score their own
topic on-topic, so the 0.40 rule never faced real risk (the first live audit found nine
of nine supported and called it a smell -- the inverse pass then materially bounded three of
them). For each SUPPORTED insight:

- Write probes engineered to find the counter case -- the defense, the alternative frame,
  the population for whom the claim does not hold -- and run them against the corpus.
- Re-read the evidence already gathered combatively: what would a skeptic quote from the
  same rows?
- Record what you find as evidence rows carrying `counter: true`,
  in the same insight, alongside the supporting rows. A bounded insight keeps its proof and
  gains its limits: `status` moves to `bounded`, the bound is written into the insight's
  `description` (one sentence naming where the claim does not hold), the `(COUNTER)` rows
  carry its evidence, and any axis the bound invalidates -- usually frequency or fit --
  gets re-scored. An insight the counter-probes gut gets re-scored wholesale, contradiction
  (`status: "contradicted"`) included; an insight that survives untouched is `supported`.

The pass is not optional and not a formality: a Discovery landed with zero counter-evidence
across every supported insight should read as suspicious to its own author.

## Land the document

The Discovery schema requires `meta` (with `title`, `threshold`, `status`, `date`, and
`author` -- status enum draft/review/done, `author` the invoking agent), the five-axis
`weights`, and `insights` (each with `id`, `label`, `description`, `personas`, `scores`,
its `status` verdict, and structured `evidence`; `meta.saturation` rides when the
instrument supplies one); resolve the current schema by its keyword rather than assuming:
`legion document list --doc-type schema --json` and take the row whose payload carries
`"x-doc-type": "discovery"`. Validate before create, then create:

```
legion document validate --schema <schema-id> --file discovery.json
legion document create --doc-type discovery --owner <agent> --surface <surface> --from discovery.json
```

`--surface` is the service surface -- the same surface the intent document carries (the
product name, not a git repo). The store refuses a schema violation on every path, so a
refusal here means the payload is wrong, not that the gate is optional. Contradicted claims
appear in the document as contradicted insights with their disconfirming score --
deleting them would erase the finding.

## Refuses

- Treating an empty query result as disconfirmation.
- Landing a FINAL Discovery whose supported insights never faced a counter-probe -- the inverse
  pass is a step, not a suggestion. (An orientation draft parks without it; its supported
  insights are provisional.)
- Softening a sub-0.40 contradiction, or omitting a contradicted insight from the document.
- Carrying evidence anywhere except this Discovery -- downstream artifacts cite insights,
  they do not re-argue evidence.
- Waiting synchronously on a crawl: a crawl in flight is a park, never a blocked session.
