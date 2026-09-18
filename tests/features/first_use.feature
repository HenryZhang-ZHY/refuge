Feature: First use with a local Git remote and a OneDrive sync folder
  Refuge hosts the live bare repository outside OneDrive and publishes immutable,
  verified snapshots into a directory that the OneDrive client is responsible for uploading.

  @S01
  Scenario: Configure local repositories and a OneDrive backup target
    Given a first-time user has a company OneDrive sync folder
    When they initialize Refuge with a separate local repository directory
    Then Refuge stores both resolved paths in its configuration
    And Refuge explains that cloud upload is not verified
    And Refuge shows the next command needed to create a repository

  @S02
  Scenario: Create a repository and connect an existing working copy
    Given Refuge has been initialized
    When the user creates a hosted repository named "notes"
    Then Refuge creates a bare repository outside OneDrive
    And Refuge prints a copyable "git remote add refuge" command

  @S03
  Scenario: A push automatically publishes a verified local snapshot
    Given the "notes" working copy uses the Refuge repository as a remote
    When the user commits a change and pushes the main branch
    Then the push succeeds without a separate backup command
    And a verified bundle and manifest appear in the OneDrive sync folder
    And status says the repository is protected locally without claiming cloud confirmation

  @S04
  Scenario: A later push publishes a new generation without deleting the old one
    Given the "notes" repository has an initial snapshot and one pushed snapshot
    When the user commits and pushes another change
    Then Refuge publishes generation 3 automatically
    And earlier generations remain available
    And the newest manifest covers the repository's current refs

  @S05
  Scenario: Restore from the OneDrive folder on a clean Refuge installation
    Given the original Refuge repository directory is unavailable
    And a clean Refuge installation points at the existing OneDrive target
    When the user restores the "notes" repository
    Then the repository identity and refs match the published snapshot
    And the restored repository can be cloned as a normal Git remote

  @S06
  Scenario: Default restore falls back from a corrupted newest snapshot
    Given the "notes" repository has a corrupted newest snapshot and a valid older snapshot
    And a clean Refuge installation points at the existing OneDrive target
    When the user restores the "notes" repository
    Then the repository identity and refs match the published snapshot
    And Refuge reports the skipped snapshot and actual restored snapshot

  @S07
  Scenario: Explicit restore never substitutes for a corrupted snapshot
    Given the "notes" repository has a corrupted newest snapshot and a valid older snapshot
    And a clean Refuge installation points at the existing OneDrive target
    When the user explicitly restores the corrupted snapshot
    Then restore fails without creating a repository

  @S08
  Scenario: Restoring under another name cannot duplicate an active identity
    Given the "notes" working copy uses the Refuge repository as a remote
    When the user commits a change and pushes the main branch
    And the user restores "notes" as "notes-copy"
    Then Refuge rejects the duplicate active repository identity
