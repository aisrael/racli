Feature: racli find-definition

  Scenario: text find-definition resolves the mkfifo call to its declaration
    When the following command is run:
      """
      racli find-definition fixtures/queue/src/main.rs --line 80 --character 21 --text
      """
    Then it should exit with status code 0
    And stdout should contain "sys.rs"

  Scenario: JSON find-definition resolves the mkfifo call to its declaration
    When the following command is run:
      """
      racli find-definition fixtures/queue/src/main.rs --line 80 --character 21
      """
    Then it should exit with status code 0
    And the JSON output should match JSONPath "$[*].uri" with a value ending with "sys.rs"
