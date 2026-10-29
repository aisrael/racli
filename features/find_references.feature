Feature: racli find-references

  Scenario: text find-references includes the declaration and the call site
    When the following command is run:
      """
      racli find-references --path fixtures/queue/src/sys.rs --line 3 --character 8 --text
      """
    Then it should exit with status code 0
    And the output should contain "sys.rs"
    And the output should contain "main.rs"

  Scenario: JSON find-references includes the call site
    When the following command is run:
      """
      racli find-references --path fixtures/queue/src/sys.rs --line 3 --character 8 --json
      """
    Then it should exit with status code 0
    And the JSON output should match JSONPath "$[*].uri" with a value ending with "main.rs"
