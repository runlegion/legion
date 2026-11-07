# Graph versus relational toy: predictions

Written 2026-11-07, before anything is built, after the worker toy measured rannoch's fill at about 5,000 rannoch events a second on Westwind (4,846 to 6,172 across runs), against the roughly 80,000 its predictions expected from flat benchmarks. It runs in the worker toy's tree (`toys/workers` on rannoch main fabf9fd), on the same machine, with nothing else building.

## The question

Is Westwind slow in rannoch because it is a graph, or because of how rannoch stores one? The same events and the same scenarios run three ways, and every way must give the same answers.

## The three versions

1. **Graph.** The worker toy's store as built: each row is a node with no foreign-key properties, and each foreign key is an edge (`rannoch.link.<relation>`). This is the path measured at about 5,000 a second.
2. **Nodes only.** The same rows into rannoch as nodes that keep their foreign keys as plain properties, with no edges. It isolates what the edges cost.
3. **Relational.** The same rows into SQLite tables shaped like Westwind's own schema (`westwind/schema.sql`), with an index on every foreign-key column, written in batched transactions. No rannoch. It is the floor: what the same data costs on the same machine and disk without a graph store.

## The data and scenarios

The seed (1,496 events) for correctness, and the worker toy's extended log (1,754,619 events, 1,000,788 order lines, fixed seed) for speed. The same events in the same order for all three.

Scenarios, each answered by all three versions and compared with SQL over the seed:

- A. `stock.low`: the lines that take a product under its reorder level (the worker toy's 19 on the seed).
- B. Unshipped: orders without a shipped date 30 days after placing (37 on the seed).
- C. One hop: every line of one order, with its product.
- D. Two hops: every product one customer has ordered.
- E. Recursive: an employee's whole reporting chain, and everyone under one manager.
- F. Fan-in: every customer who ordered from one supplier, through line, product and supplier.

## Predictions

1. All three versions give the same answers for A to F on the seed and on the extended log.
2. Relational loads the extended log at 150,000 rows a second or more.
3. Edges are most of the graph's cost: nodes-only loads at least 2 times faster than graph.
4. Graph loads within 10 times of relational. Worse than that, and the cost is in rannoch's store, not in the shape of the data.
5. One-hop and two-hop reads (C, D) answer in under 1 ms on relational and under 10 ms on graph, on the extended log.
6. The recursive and fan-in reads (E, F) answer faster on graph than on nodes-only, whose foreign keys are plain properties with no index.
7. Each version's on-disk size after the extended log is recorded, and graph is no more than 5 times relational.

## What breaks the idea

- The versions disagree on any answer.
- Graph loads more than 10 times slower than relational.
- Graph reads lose to nodes-only on the traversals (E, F), which are what edges are for.
