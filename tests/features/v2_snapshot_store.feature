Feature: Immutable incremental v2 snapshot storage
  Refuge publishes recovery points as a checkpoint and delta chain, reuses
  content-addressed data, and exposes deterministic verification and usage.

  Background:
    Given a v2 repository named "notes" has one protected commit

  @V201
  Scenario: Ordinary pushes append an incremental delta
    When the user pushes a second commit
    Then the published recovery points are empty, checkpoint, and delta
    And the repository remains protected

  @V202
  Scenario: Manual backup is idempotent unless a checkpoint is requested
    When the user backs up unchanged refs
    Then Refuge reports that the repository is already protected
    And no new manifest is published
    When the user forces a checkpoint backup
    Then a new checkpoint manifest is published

  @V203
  Scenario: A new branch on an existing commit is refs-only
    When the user adds a branch at an existing commit and backs up
    Then the newest recovery point is refs-only
    And deep verification succeeds

  @V204
  Scenario: Deep verification detects same-size corruption
    Given the newest bundle is corrupted without changing its size
    When the user deeply verifies the repository
    Then verification reports invalid with exit code 1
    And shallow snapshot listing still reports valid

  @V205
  Scenario: Usage explains repository storage
    When the user asks for snapshot usage
    Then usage reports checkpoints, LFS objects, manifests, and total bytes

  @V206
  Scenario: A conflicting manifest copy makes status corrupt
    Given a conflicting file appears in the snapshots directory
    When the user checks repository status
    Then status reports corruption and names the conflicting file

  @V207
  Scenario: A corrupt delta is healed by the next checkpoint
    Given the newest delta bundle is truncated
    When the user pushes another commit
    Then Refuge publishes a checkpoint and protection is restored

  @V208
  Scenario: The synchronized target contains only final immutable files
    When the user pushes a second commit
    Then the target contains no locks, staging directories, or partial files
