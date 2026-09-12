Feature: CLI basics

  Scenario: racli version prints client and server version
    When the following command is run:
      """
      racli version
      """
    Then it should exit with status code 0
    And stdout should contain "client: "
    And stdout should contain "server: "

  Scenario: racli --help lists the subcommands
    When the following command is run:
      """
      racli --help
      """
    Then it should exit with status code 0
    And stdout should contain "search"
    And stdout should contain "find-definition"
    And stdout should contain "find-references"
