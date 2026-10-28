Feature: racli search

  Scenario: text search finds a known symbol
    When the following command is run:
      """
      racli search mkfifo --text
      """
    Then it should exit with status code 0
    And the output should contain "mkfifo"
    And the output should contain "sys.rs"

  Scenario: JSON search finds a known symbol
    When the following command is run:
      """
      racli search mkfifo --json
      """
    Then it should exit with status code 0
    And the JSON output should match JSONPath "$[?(@.name=='mkfifo')]"

  Scenario: fuzzy search also matches similar names
    When the following command is run:
      """
      racli search name --kind all-symbols --text
      """
    Then it should exit with status code 0
    And the output should contain "Named [INTERFACE]"

  Scenario: --exact keeps only symbols named exactly as the query
    When the following command is run:
      """
      racli search name --kind all-symbols --exact --text
      """
    Then it should exit with status code 0
    And the output should be
      """
      flat: 2 symbol(s)
      name [FUNCTION] fixtures/queue/src/sys.rs:38:8
      name [FUNCTION] fixtures/queue/src/sys.rs:32:8
      """

  Scenario: --exact applies to each alternative pattern
    When the following command is run:
      """
      racli search name|unlink --kind all-symbols --exact --json
      """
    Then it should exit with status code 0
    And the JSON output should be an array of 4 items

  Scenario: --exact keeps a module path's symbol
    When the following command is run:
      """
      racli search sys::mkfifo --kind all-symbols --exact --json
      """
    Then it should exit with status code 0
    And the JSON output should be an array of 1 item

  Scenario: --exact with only a partial name finds nothing
    When the following command is run:
      """
      racli search mkfif --kind all-symbols --exact --json
      """
    Then it should exit with status code 0
    And the JSON output should be an array of 0 items
