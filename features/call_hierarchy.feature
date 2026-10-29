Feature: racli call-hierarchy

  Scenario: text call-hierarchy shows the caller
    When the following command is run:
      """
      racli call-hierarchy --path fixtures/queue/src/sys.rs --line 3 --character 8 --direction incoming --text
      """
    Then it should exit with status code 0
    And the output should contain "mkfifo"
    And the output should contain "main.rs"

  Scenario: text call-hierarchy outgoing shows the callee
    When the following command is run:
      """
      racli call-hierarchy --path fixtures/queue/src/sys.rs --line 3 --character 8 --direction outgoing --text
      """
    Then it should exit with status code 0
    And the output should contain "-> callees:"

  Scenario: JSON call-hierarchy incoming includes the caller
    When the following command is run:
      """
      racli call-hierarchy --path fixtures/queue/src/sys.rs --line 3 --character 8 --direction incoming --json
      """
    Then it should exit with status code 0
    And the JSON output should match JSONPath "$.incoming[*].uri" with a value ending with "main.rs"
