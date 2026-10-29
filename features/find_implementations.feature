Feature: racli find-implementations

  Scenario: text find-implementations resolves the Named trait to its Fifo implementation
    When the following command is run:
      """
      racli find-implementations --path fixtures/queue/src/sys.rs --line 31 --character 11 --text
      """
    Then it should exit with status code 0
    And the output should contain "sys.rs"
