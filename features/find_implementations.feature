Feature: racli find-implementations

  Scenario: text find-implementations resolves the Named trait to its Fifo implementation
    When the following command is run:
      """
      racli find-implementations fixtures/queue/src/sys.rs --line 29 --character 10 --text
      """
    Then it should exit with status code 0
    And the output should contain "sys.rs"

  Scenario: JSON find-implementations resolves the Named trait to its Fifo implementation
    When the following command is run:
      """
      racli find-implementations fixtures/queue/src/sys.rs --line 29 --character 10
      """
    Then it should exit with status code 0
    And the JSON output should match JSONPath "$[*].uri" with a value ending with "sys.rs"
