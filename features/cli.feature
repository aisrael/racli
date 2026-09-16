Feature: CLI basics

  Scenario: racli version prints client and server version
    When the following command is run:
      ```
      racli version
      ```
    Then it should exit with status code 0
    And the output should be
      ```
      client: 0.2.2
      server: 0.2.2
      rust-analyzer: 1.95.0 (59807616 2026-04-14)
      ```

  Scenario: racli --help lists the subcommands
    When the following command is run:
      ```
      racli --help
      ```
    Then it should exit with status code 0
    And the output should contain "search"
    And the output should contain "find-definition"
    And the output should contain "find-references"
    And the output should contain "find-implementations"
    And the output should contain "call-hierarchy"
