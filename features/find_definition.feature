Feature: racli find-definition

  Scenario: text find-definition resolves the mkfifo call to its declaration
    When the following command is run:
      """
      racli find-definition --path fixtures/queue/src/main.rs --line 81 --character 22 --text
      """
    Then it should exit with status code 0
    And the output should contain "sys.rs"

  Scenario: JSON find-definition resolves the mkfifo call to its declaration
    When the following command is run:
      """
      racli find-definition --path fixtures/queue/src/main.rs --line 81 --character 22
      """
    Then it should exit with status code 0
    And the JSON output should match JSONPath "$[*].uri" with a value ending with "sys.rs"

  Scenario: --source prints a definition's whole source
    When the following command is run:
      """
      racli find-definition sys::mkfifo --source --text
      """
    Then it should exit with status code 0
    And the output should be
      """
      mkfifo [FUNCTION] fixtures/queue/src/sys.rs:3:8
        fixtures/queue/src/sys.rs:3:8: pub fn mkfifo(path: &str, mode: u16) -> i32 {
          3  pub fn mkfifo(path: &str, mode: u16) -> i32 {
          4      let c_path = CString::new(path).unwrap();
          5      let p_path = c_path.as_ptr();
          6      unsafe {
          7          return raw::mkfifo(p_path, mode as libc::mode_t);
          8      }
          9  }
      """

  Scenario: --source includes the doc comment
    When the following command is run:
      """
      racli find-definition Named --source --text
      """
    Then it should exit with status code 0
    And the output should contain "30  /// Names a queue."
    And the output should contain "31  pub trait Named {"

  Scenario: --hover prints the signature and doc comment
    When the following command is run:
      """
      racli find-definition Named --hover --text
      """
    Then it should exit with status code 0
    And the output should contain "pub trait Named"
    And the output should contain "Names a queue."

  Scenario: JSON with --hover and --source has hover and source fields
    When the following command is run:
      """
      racli find-definition Named --hover --source
      """
    Then it should exit with status code 0
    And the JSON output should match JSONPath "$[0].result[0].hover"
    And the JSON output should match JSONPath "$[0].result[0].source.text"

  Scenario: JSON without --hover or --source has neither field
    When the following command is run:
      """
      racli find-definition Named
      """
    Then it should exit with status code 0
    And the JSON output should match JSONPath "$[0].result[0].uri" with a value ending with "sys.rs"
    And the JSON output should not match JSONPath "$[0].result[0].hover"
    And the JSON output should not match JSONPath "$[0].result[0].source"

