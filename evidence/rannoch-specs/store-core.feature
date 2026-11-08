Feature: The core store
  Rannoch's core store, from store-core.md and trash.md: one write per fact, append-only,
  history on each class's declared axis, the trash write, and next by type.

  # STORE-1
  Scenario: An unchanged fact writes nothing, a changed one appends a version
    Given a fact already current at its axis point
    When the same fact arrives again, then a changed one arrives
    Then the first writes nothing
    And the second appends one version

  # STORE-2
  Scenario: No version or link version is ever deleted or updated
    Given any sequence of writes, endings, corrections and trashes
    When the store runs them
    Then no statement deletes or updates a version, an ending or a correction

  # STORE-3
  Scenario: The current row is the highest axis point, then the latest known-at
    Given two different facts for one key at the same axis point, known at different times
    When the current row is read
    Then it is the one known later
    And the result is the same whatever order they arrived in

  # STORE-4
  Scenario: A link's ending and correction are more link versions
    Given a link
    When it is ended, and another is corrected
    Then each is recorded as one more link version, and the link as written is still readable before it

  # STORE-5, TRASH-2, TRASH-3 (trash is delete)
  Scenario: A trashed node never shows up again
    Given a node with links to and from it
    When a rannoch.trashed event for it is committed
    Then the current read of the node returns nothing
    And every link to and from it reads ended from the trash's point on each relation's axis
    And a read as of a point before the trash sees the node and its links as they were

  # STORE-5
  Scenario: A trash is never refused by a relation's axis
    Given a node whose class's axis differs from one of its relations' axes
    When it is trashed
    Then the trash succeeds and the node's links end

  # STORE-6
  Scenario: History reads answer from the versions
    Given a fact with versions at several axis points and known-ats
    When it is read as of an axis point, and as known at a time
    Then each read returns the version current at that point and time

  # STORE-9
  Scenario: An amendment is refused until the amend processor exists
    Given the core store
    When a rannoch.amended event arrives
    Then it is refused with a reason naming rannoch-processor-amend

  # #243 (next by type, toy way 1)
  Scenario: A worker is handed only the committed events of its types, by seek
    Given a log of committed events of several types
    When a worker asks next_of for one type
    Then it is handed the next committed event of that type after its place
    And nothing is written for events of other types
    And the lookup seeks by the type index
