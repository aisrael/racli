Feature: racli find-references

  Scenario: text find-references includes the declaration and the call site
    When the following command is run:
      """
      racli find-references fixtures/queue/src/sys.rs --line 2 --character 7 --text
      """
    Then it should exit with status code 0
    And stdout should contain "sys.rs"
    And stdout should contain "main.rs"

  Scenario: JSON find-references includes the call site
    When the following command is run:
      """
      racli find-references fixtures/queue/src/sys.rs --line 2 --character 7
      """
    Then it should exit with status code 0
    And the JSON output should match JSONPath "$[*].uri" with a value ending with "main.rs"
