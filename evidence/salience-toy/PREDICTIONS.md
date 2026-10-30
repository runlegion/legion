# Salience toy: predictions (written before the run)

Data: legion's live reflections (legion.db, read-only), 2026-10-30. Rafters: 4,627 memories, 336 recalled at least once; March, its first month, holds 821 memories and 110 recalls, the most of any month.

Three rules, each giving a salience in (0, 1], with a floor of 0.05 below which a relay drops a memory (forgotten in practice, still in the record):

- **A, age:** decays from creation, half-life 30 days. No use term.
- **B, use:** decays from last use (last recall, else creation), half-life 30 days, times (1 + 0.1 x recall count), capped at 1.
- **C, use with keepers:** B, but identity, workflow and evergreen memories never decay.

Predictions:

- **P1:** Under A, rafters' March memories recalled at least twice rank in the bottom half of rafters. Under B they rank in the top 10%.
- **P2:** Under B, at least 90% of rafters memories recalled at least twice stay above the floor. Under A, under 10% do.
- **P3:** Under B, at least 80% of never-recalled memories older than 60 days fall below the floor.
- **P4:** Under B, some identity or workflow memories fall below the floor. Under C, none do.
- **P5:** Under C, under 25% of all memories stay above the floor.

Caveat, stated before the run: recall counts are both an input to B and C and the evidence that a memory matters, so this shows how the rules behave on real data. It does not show that they predict future use; that needs recall events with times, which legion does not keep (only a count and the last time).
