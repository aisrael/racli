Feature: racli position commands by symbol name

  The position commands take symbol names as positional arguments, as well as,
  or instead of, a position given with --path, --line and --character. A name
  can be qualified by a module path from the crate root (sys::mkfifo) or by the
  type or trait that contains it (Fifo::name). Each definition gets its own
  result.

  Scenario: text find-references by name includes the declaration and the call site
    When the following command is run:
      """
      racli find-references sys::mkfifo --text
      """
    Then it should exit with status code 0
    And the output should contain "mkfifo [FUNCTION]"
    And the output should contain "main.rs"

  Scenario: a module path picks one of two definitions with the same name
    When the following command is run:
      """
      racli find-references sys::mkfifo --text
      """
    Then it should exit with status code 0
    And the output should be
      """
      mkfifo [FUNCTION] fixtures/queue/src/sys.rs:3:8
        fixtures/queue/src/main.rs:81:22:         let _ = sys::mkfifo(PATH, 0o666);
        fixtures/queue/src/sys.rs:3:8: pub fn mkfifo(path: &str, mode: u16) -> i32 {
      """

  Scenario: an ambiguous name gives a result for each definition
    When the following command is run:
      """
      racli find-references mkfifo --text
      """
    Then it should exit with status code 0
    And the output should contain "mkfifo [FUNCTION] fixtures/queue/src/sys.rs:3:8"
    And the output should contain "mkfifo [FUNCTION] fixtures/queue/src/sys.rs:25:16"

  Scenario Outline: a type or trait qualifies a method
    When the following command is run:
      """
      racli find-references <name> --text
      """
    Then it should exit with status code 0
    And the output should contain "name [FUNCTION] in <container> "

    Examples:
      | name        | container |
      | Fifo::name  | Fifo      |
      | Named::name | Named     |

  Scenario: several names give a result for each
    When the following command is run:
      """
      racli find-definition Fifo Named --text
      """
    Then it should exit with status code 0
    And the output should be
      """
      Fifo [STRUCT] fixtures/queue/src/sys.rs:35:12
        fixtures/queue/src/sys.rs:35:12: pub struct Fifo;
      Named [INTERFACE] fixtures/queue/src/sys.rs:31:11
        fixtures/queue/src/sys.rs:31:11: pub trait Named {
      """

  Scenario: a definition two names find is run once
    When the following command is run:
      """
      racli find-references mkfifo sys::mkfifo --text
      """
    Then it should exit with status code 0
    And the output should contain "mkfifo [FUNCTION] fixtures/queue/src/sys.rs:3:8"
    And the output should contain "mkfifo [FUNCTION] fixtures/queue/src/sys.rs:25:16"

  Scenario: a position given with a name is labelled with its symbol
    When the following command is run:
      """
      racli find-references Fifo --path fixtures/queue/src/main.rs --line 86 --character 32 --text
      """
    Then it should exit with status code 0
    And the output should contain "Fifo [STRUCT] fixtures/queue/src/sys.rs:35:12"
    And the output should contain "name [FUNCTION] in Fifo fixtures/queue/src/sys.rs:38:8"

  Scenario: a position whose symbol a name already found is run once
    When the following command is run:
      """
      racli find-references mkfifo --path fixtures/queue/src/sys.rs --line 3 --character 8 --text
      """
    Then it should exit with status code 0
    And the output should contain "mkfifo [FUNCTION] fixtures/queue/src/sys.rs:3:8"
    And the output should contain "mkfifo [FUNCTION] fixtures/queue/src/sys.rs:25:16"

  Scenario: an unknown name lists the closest symbols
    When the following command is run:
      """
      racli find-references mkfif
      """
    Then it should exit with status code 0
    And the error output should contain "no symbol named mkfif; closest:"
    And the error output should contain "mkfifo [FUNCTION]"

  Scenario: an unknown name doesn't stop the others
    When the following command is run:
      """
      racli find-definition Fifo mkfif --text
      """
    Then it should exit with status code 0
    And the error output should contain "no symbol named mkfif"
    And the output should contain "Fifo [STRUCT] fixtures/queue/src/sys.rs:35:12"

  Scenario: a position in a file that doesn't exist doesn't stop the names
    When the following command is run:
      """
      racli find-definition Fifo --path fixtures/queue/src/nope.rs --line 3 --character 8 --text
      """
    Then it should exit with status code 0
    And the error output should contain "cannot canonicalize fixtures/queue/src/nope.rs"
    And the output should contain "Fifo [STRUCT] fixtures/queue/src/sys.rs:35:12"

  Scenario: --kind keeps only definitions of that kind
    When the following command is run:
      """
      racli find-definition Fifo --kind struct --text
      """
    Then it should exit with status code 0
    And the output should be
      """
      Fifo [STRUCT] fixtures/queue/src/sys.rs:35:12
        fixtures/queue/src/sys.rs:35:12: pub struct Fifo;
      """

  Scenario: --glob keeps only definitions in files it matches
    When the following command is run:
      """
      racli find-references unlink --glob fixtures/queue/src/*.rs --text
      """
    Then it should exit with status code 0
    And the output should contain "unlink [FUNCTION] fixtures/queue/src/sys.rs:11:8"
    And the output should contain "unlink [FUNCTION] fixtures/queue/src/sys.rs:26:16"

  Scenario: --glob that leaves out every definition lists them
    When the following command is run:
      """
      racli find-references mkfifo --glob fixtures/queue/src/main.rs
      """
    Then it should exit with status code 0
    And the error output should contain "no symbol named mkfifo passes --kind and --glob; found:"

  Scenario: find-implementations by name finds a trait's impls
    When the following command is run:
      """
      racli find-implementations Named --text
      """
    Then it should exit with status code 0
    And the output should contain "Named [INTERFACE]"
    And the output should contain "fixtures/queue/src/sys.rs:37:16: impl Named for Fifo {"

  Scenario: call-hierarchy by name shows the caller
    When the following command is run:
      """
      racli call-hierarchy sys::mkfifo --direction incoming --text
      """
    Then it should exit with status code 0
    And the output should contain "main (FUNCTION) fixtures/queue/src/main.rs:58:4"

  Scenario Outline: arguments that don't make a name or a position are usage errors
    When the following command is run:
      """
      racli find-references <args>
      """
    Then it should exit with status code 2
    And the error output should contain "<error>"

    Examples:
      | args                                                                    | error                                        |
      | fixtures/queue/src/sys.rs --line 3 --character 8                        | use --path fixtures/queue/src/sys.rs         |
      | --path fixtures/queue/src/sys.rs                                        | --line                                       |
      | mkfifo --line 3 --character 8                                           | --path                                       |
      | --kind function --path fixtures/queue/src/sys.rs --line 3 --character 8 | --kind and --glob only apply to symbol names |
      | mkfifo --glob '[unclosed'                                               | invalid value                                |
      |                                                                         | <NAME>                                       |
