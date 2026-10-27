Feature: CLI basics

  Scenario: racli version prints client and server version
    When the following command is run:
      ```
      racli version
      ```
    Then it should exit with status code 0
    And the output should contain "client: 0.2.0"
    And the output should contain "server: 0.2.0"
    And the output should contain "rust-analyzer: "

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
