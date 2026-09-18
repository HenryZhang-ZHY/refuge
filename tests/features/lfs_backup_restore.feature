Feature: Git LFS objects survive backup and restore
  A repository that tracks large files with Git LFS must be fully
  recoverable from a clean machine: both its Git history and its LFS
  object content, not just the former.

  @LFS01
  Scenario: A push with Git LFS objects is backed up and restored intact
    Given the "vault" working copy uses the Refuge repository as a remote
    And the working copy tracks "*.bin" files with Git LFS
    When the user commits a large binary file and pushes the main branch
    Then the push succeeds without a separate backup command
    And the push output reports LFS bytes protected locally
    And status says the repository is protected locally
    And a verified LFS archive appears in the OneDrive sync folder
    When a clean Refuge installation restores the "vault" repository
    Then the restored repository's Git LFS objects match the original content

  @LFS02
  Scenario: Missing LFS content blocks protection until the object is supplied
    Given the "vault" working copy uses the Refuge repository as a remote
    And the working copy tracks "*.bin" files with Git LFS
    When the user pushes a large binary pointer without uploading its LFS object
    Then the Git push succeeds but Refuge reports incomplete LFS protection
    And status says the repository has pending unprotected changes
    When the user uploads the missing LFS object and retries backup
    Then the retry protects the Git history and LFS content
