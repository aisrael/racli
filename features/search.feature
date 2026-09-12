Feature: racli search

  Scenario: text search finds a known symbol
    When the following command is run:
      """
      racli search mkfifo --text
      """
    Then it should exit with status code 0
    And stdout should contain "mkfifo"
    And stdout should contain "sys.rs"

  Scenario: JSON search finds a known symbol
    When the following command is run:
      """
      racli search mkfifo --json
      """
    Then it should exit with status code 0
    And the JSON output should match JSONPath "$[?(@.name=='mkfifo')]"
