Feature: Lanes
  Rannoch's lanes, from lanes.md. Every lane meets the Lane contract in rannoch-queue;
  which crates supply the lanes, and which types go to which lane, are fixed at build.

  # LANES-2
  Scenario: Every lane meets the Lane contract
    Given a lane supplied by rannoch-queue-mem, rannoch-queue-trickle or rannoch-queue-ring
    When a worker calls next, done, failed and position through the Lane trait
    Then each lane answers with the signatures rannoch-queue defines

  # LANES-3
  Scenario: A lane's types are fixed at compile time
    Given a trickle lane built for the types in a Types constant
    When the program runs
    Then nothing about the lane's types is read at run time

  # LANES-6
  Scenario: A slow lane hands out only committed events of its types, in order
    Given a log holding committed events of type "slow" among events of other types
    And a trickle lane built for "slow"
    When a worker takes events from the lane until none are left
    Then it is handed exactly the committed "slow" events, in log order
    And it is handed no event of another type

  # LANES-6
  Scenario: A slow lane never hands out an uncommitted event
    Given an event of type "slow" accepted but not yet committed by the heartbeat
    When a worker of a trickle lane built for "slow" asks for its next event
    Then it is not handed that event

  # LANES-6
  Scenario: A slow lane resumes from the worker's place
    Given a worker of a trickle lane has taken and closed some events and left one in progress
    When the program restarts and the worker asks for its next event
    Then it is handed the event it left in progress, then the events after its place

  # LANES-6
  Scenario: A slow lane writes nothing for events it skips
    Given a log where most committed events are of types the lane does not handle
    When a worker of the lane takes its events
    Then no row is written for any event it skips

  # LANES-7 (the part a lane itself must meet)
  Scenario: A slow lane writes no facts
    Given a trickle lane
    When a worker takes and closes events through it
    Then the lane writes no fact and no event to the store

  # LANES-5
  Scenario: The ring takes its events from memory and commits a whole frame per transaction
    Given a fast lane supplied by rannoch-queue-ring
    When events are accepted and a frame is committed
    Then the whole frame is written in one SQLite transaction
    And the lane never reads the log

  # LANES-5, LANES-9
  Scenario: The ring behaves as the memory queue does for a worker
    Given the queue tests of rannoch-queue-mem
    When they run against rannoch-queue-ring, changed only in how the queue is constructed
    Then they pass
