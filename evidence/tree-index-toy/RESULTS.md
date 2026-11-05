# Tree index toy: results

Run 2026-11-05 with `rust-analyzer scip`, the indexer `legion index` falls back to for Rust, on rannoch's main checkout and on the worktree `rannoch-wt/170` (branch `feat/170-trim`, never merged), each into a scratch file.

| # | Prediction | Result | Verdict |
|---|---|---|---|
| 1 | A tree indexes in under 60 s | main 11 s, tree 10 s | Held |
| 2 | Tree index within 20% of main's size | tree 3,548,044 bytes, main 1,536,072 | Missed, explained below |
| 3 | A branch-only symbol is in the tree index, not main | `Retention`: 124 occurrences in the tree, 0 in main | Held |
| 4 | The stored main index is unchanged | same size and update time before and after | Held |

The scratch index of main is byte-for-byte the stored size (1,536,072), so the method matches what `legion index` stores.

The size miss is the branch, not the method: `feat/170-trim` was cut from main before the rollback to v0.5.0 (#211), so it still holds the relay and purge code that main no longer has. A tree's index is the size of its own code.

Neither thing that would break the idea happened: a tree indexes in about 10 seconds, and the indexer takes any path, so pointing it at a tree needs no indexer change, only legion choosing the path and the key it stores under.
