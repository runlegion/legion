# Tree index toy: predictions

Written 2026-11-05, before any run.

## The question

Is an index per tree cheap enough to build when a worktree is made, and does it answer for the branch where the main index answers for the main checkout?

## Method

Index one rannoch worktree whose branch differs from main, with the same indexer `legion index` runs, into a file of its own. Leave the stored main index alone. Compare the two: time, size, and whether a symbol that exists only on the branch is in the tree index and not the main one.

## Predictions

1. Indexing a rannoch tree takes under 60 seconds on this machine, about what `legion index rannoch` takes for the main checkout.
2. The tree index is within 20% of the main index's size.
3. A symbol added only on the branch is found in the tree index and not in the main index.
4. The stored main index is unchanged afterwards: same hash, same update time.

## What breaks the idea

- A tree takes minutes, so building one per worktree would hold up every builder's start.
- The indexer cannot be pointed at a path other than the registered checkout without changing legion.
