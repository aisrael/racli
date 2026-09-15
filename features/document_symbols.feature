Feature: racli document-symbols

  Scenario: text document-symbols lists top-level and nested symbols
    When the following command is run:
      """
      racli document-symbols fixtures/queue/src/sys.rs --text
      """
    Then it should exit with status code 0
    And the output should contain "mkfifo"
    And the output should contain "unlink"
    And the output should contain "raw"

  Scenario: JSON document-symbols includes symbol names
    When the following command is run:
      """
      racli document-symbols fixtures/queue/src/sys.rs
      """
    Then it should exit with status code 0
    And the JSON output should match JSONPath "$[*].name" with a value ending with "mkfifo"

  Scenario: --output-format json matches --json
    When the following command is run:
      """
      racli document-symbols fixtures/queue/src/sys.rs --output-format json
      """
    Then it should exit with status code 0
    And the JSON output should match JSONPath "$[*].name" with a value ending with "unlink"
