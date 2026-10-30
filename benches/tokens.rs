//! Measures how many tokens each answer puts into an agent's context: `racli` (the CLI calling a
//! running `racli server`) against the `grep` and `sed` pipelines an agent would run without it,
//! for `find-definition`, `find-references`, `find-implementations` and `call-hierarchy`. The
//! symbols, the commands and the workspace come from a YAML file: the one that `RACLI_BENCH_CONFIG`
//! names, or `benches/config/racli.yaml` (racli's own source tree). It prints Markdown
//! tables and writes `results.csv`, `grep-only.txt`, `grep-missed.txt` and a token cache to
//! `target/<name>/`.

#![cfg_attr(not(unix), allow(dead_code))]

#[allow(dead_code)]
mod support;

use std::collections::BTreeSet;
use std::collections::HashMap;
use std::fs;
use std::hash::DefaultHasher;
use std::hash::Hash;
use std::hash::Hasher;
use std::io::Write;
use std::path::Path;
use std::path::PathBuf;
use std::process::Command;
use std::process::Stdio;
use std::thread;
use std::time::Duration;
use std::time::Instant;

use serde::Deserialize;
use serde_json::json;

/// A `path:line` pair, as grep and racli print it.
type Location = (String, u32);

/// The model whose `count_tokens` endpoint measures the outputs.
const MODEL: &str = "claude-sonnet-5-5";

/// The environment variable that holds the Anthropic API key.
const API_KEY_VAR: &str = "ANTHROPIC_API_KEY";

/// The environment variable that, set to `estimate`, skips the endpoint even when a key is set.
const TOKENS_VAR: &str = "RACLI_BENCH_TOKENS";

/// The environment variable that names the YAML configuration file.
const CONFIG_VAR: &str = "RACLI_BENCH_CONFIG";

/// The configuration file to read when [`CONFIG_VAR`] is not set, under the manifest directory.
const DEFAULT_CONFIG: &str = "benches/config/racli.yaml";

/// What racli prints on standard error when `--symbol-search-limit` cut the results.
const LIMIT_WARNING: &str = "symbol-search-limit";

/// The heading of each Markdown results table.
const TABLE_HEADER: &str = "| Symbol | Variant | Lines | Bytes | Tokens | Precision | Recall | Tokens per correct location |\n| --- | --- | ---: | ---: | ---: | ---: | ---: | ---: |";

/// The benchmark's settings, read from the YAML file.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Config {
    /// The directory under `target/` that receives the results.
    name: String,
    /// The directory where `grep` and racli run. A relative path starts at the racli repository,
    /// and `~/` is the home directory.
    workspace: String,
    /// The directories, relative to the workspace, that every `grep` searches.
    search_dirs: Vec<String>,
    /// The options that every `grep` shares (`@OPTIONS@` in a command).
    #[serde(default = "default_grep_options")]
    grep_options: String,
    /// Lines of racli output that contain one of these are dropped.
    #[serde(default)]
    racli_ignore: Vec<String>,
    /// Environment variables to remove before the server starts.
    #[serde(default)]
    unset_env: Vec<String>,
    /// A symbol that exists in the workspace: the server is ready when a search for it answers.
    ready_symbol: String,
    /// How long to wait for the server to answer that search.
    #[serde(default = "default_ready_timeout_secs")]
    ready_timeout_secs: u64,
    /// How long to wait for racli to give a stable answer for one query.
    #[serde(default = "default_settle_timeout_secs")]
    settle_timeout_secs: u64,
    /// The lookups to measure, in the order of the report.
    lookups: Vec<LookupConfig>,
}

/// The `grep` options when the file gives none: Rust files only, and no `target/`.
fn default_grep_options() -> String {
    "--include='*.rs' --exclude-dir=target".to_string()
}

/// The time to wait for the server when the file gives none.
fn default_ready_timeout_secs() -> u64 {
    120
}

/// The time to wait for a stable answer when the file gives none.
fn default_settle_timeout_secs() -> u64 {
    60
}

impl Config {
    /// The workspace directory, with `~/` expanded and a relative path taken from the racli repository.
    fn workspace_dir(&self) -> PathBuf {
        match self.workspace.strip_prefix("~/") {
            Some(rest) => {
                PathBuf::from(std::env::var_os("HOME").expect("HOME is not set")).join(rest)
            }
            None => support::racli_workspace().join(&self.workspace),
        }
    }

    /// Whether a line of racli output is one that the file says to drop.
    fn is_ignored(&self, line: &str) -> bool {
        self.racli_ignore.iter().any(|text| line.contains(text))
    }

    /// `text` with `@NAME@`, `@OPTIONS@` and `@DIRS@` replaced.
    fn fill(&self, text: &str, symbol: &str) -> String {
        text.replace("@NAME@", symbol)
            .replace("@OPTIONS@", &self.grep_options)
            .replace("@DIRS@", &self.search_dirs.join(" "))
    }
}

/// One lookup in the file: the symbols to look up and the commands of each tool.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LookupConfig {
    /// The racli command: `find-definition`, `find-references`, `find-implementations` or `call-hierarchy`.
    name: String,
    /// The symbols to look up.
    symbols: Vec<String>,
    /// The commands to compare, for each tool.
    tools: Tools,
}

/// The commands of a lookup, for each tool.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Tools {
    /// The `grep` and `sed` pipelines.
    #[serde(default)]
    grep: Vec<ShellConfig>,
    /// The racli calls. The first is the reference that every variant is scored against.
    racli: Vec<RacliConfig>,
}

/// A shell pipeline in the file.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ShellConfig {
    /// The label in the tables.
    name: String,
    /// The script, run with `bash -c` in the workspace.
    command: CommandText,
    /// Whether each output line is a call site that is scored as the function containing it.
    #[serde(default)]
    by_function: bool,
}

/// A command in the file: one string, or a list of strings that are joined with spaces.
#[derive(Deserialize)]
#[serde(untagged)]
enum CommandText {
    /// The whole command.
    One(String),
    /// The stages of a pipeline.
    Parts(Vec<String>),
}

impl CommandText {
    /// The command as one string.
    fn text(&self) -> String {
        match self {
            CommandText::One(text) => text.clone(),
            CommandText::Parts(parts) => parts.join(" "),
        }
    }
}

/// A racli call in the file.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RacliConfig {
    /// The label in the tables.
    name: String,
    /// The arguments of the `racli` command.
    args: Vec<String>,
}

/// A racli command that has a `grep` and `sed` equivalent. Each one has its own way to read the answer.
#[derive(Clone, Copy, PartialEq)]
enum Lookup {
    /// `racli find-definition`.
    Definition,
    /// `racli find-references`.
    References,
    /// `racli find-implementations`.
    Implementations,
    /// `racli call-hierarchy --direction incoming`.
    CallHierarchy,
}

impl Lookup {
    /// The lookup that the racli subcommand `name` is, if there is one.
    fn from_name(name: &str) -> Option<Self> {
        match name {
            "find-definition" => Some(Lookup::Definition),
            "find-references" => Some(Lookup::References),
            "find-implementations" => Some(Lookup::Implementations),
            "call-hierarchy" => Some(Lookup::CallHierarchy),
            _ => None,
        }
    }
}

/// One way to answer a lookup.
struct Variant {
    /// The label in the tables.
    name: String,
    /// The command that produces the output.
    run: Run,
    /// Whether each output line is a call site that is scored as the function containing it.
    by_function: bool,
}

impl Variant {
    /// Whether the output comes from racli, the reference tool.
    fn is_racli(&self) -> bool {
        matches!(self.run, Run::Racli(_))
    }
}

/// The command behind a variant.
enum Run {
    /// A `bash -c` script run in the workspace root.
    Shell(String),
    /// The arguments of a `racli` call against the benchmark's server.
    Racli(Vec<String>),
}

/// The ways to answer `lookup` for `symbol`: the `grep` and `sed` pipelines, then racli. The
/// symbol must be a plain identifier, because it goes into shell commands.
fn variants(lookup: &LookupConfig, symbol: &str, config: &Config) -> Vec<Variant> {
    assert!(
        symbol
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_'),
        "`{symbol}` is not a plain identifier"
    );
    let shell = lookup.tools.grep.iter().map(|tool| Variant {
        name: tool.name.clone(),
        run: Run::Shell(config.fill(&tool.command.text(), symbol)),
        by_function: tool.by_function,
    });
    let racli = lookup.tools.racli.iter().map(|tool| Variant {
        name: tool.name.clone(),
        run: Run::Racli(racli_args(&tool.args, symbol)),
        by_function: false,
    });
    shell.chain(racli).collect()
}

/// `args` with `@NAME@` replaced by `symbol`.
fn racli_args(args: &[String], symbol: &str) -> Vec<String> {
    args.iter()
        .map(|arg| arg.replace("@NAME@", symbol))
        .collect()
}

/// The sums that make up one table row: one symbol, or one variant over all symbols of a lookup.
#[derive(Default)]
struct Totals {
    /// Non-empty output lines.
    lines: usize,
    /// Output size in bytes.
    bytes: usize,
    /// Output size in tokens.
    tokens: u64,
    /// Locations that the output gives, with repeats.
    answers: usize,
    /// Those locations that are in the reference set, with repeats.
    correct_answers: usize,
    /// Distinct reference locations that the output finds.
    found: usize,
    /// Distinct reference locations that racli gives.
    reference: usize,
}

impl Totals {
    /// Adds `other` to these totals.
    fn add(&mut self, other: &Totals) {
        self.lines += other.lines;
        self.bytes += other.bytes;
        self.tokens += other.tokens;
        self.answers += other.answers;
        self.correct_answers += other.correct_answers;
        self.found += other.found;
        self.reference += other.reference;
    }

    /// The share of the output's locations that are in the reference set.
    fn precision(&self) -> f64 {
        ratio(self.correct_answers, self.answers)
    }

    /// The share of the reference locations that the output finds.
    fn recall(&self) -> f64 {
        ratio(self.found, self.reference)
    }

    /// The tokens spent for each reference location found, or `None` if it found none.
    fn tokens_per_correct(&self) -> Option<f64> {
        (self.found > 0).then(|| self.tokens as f64 / self.found as f64)
    }

    /// The row for the Markdown table.
    fn markdown(&self, symbol: &str, variant: &str) -> String {
        format!(
            "| {symbol} | {variant} | {} | {} | {} | {:.0}% | {:.0}% | {} |",
            self.lines,
            self.bytes,
            self.tokens,
            self.precision() * 100.0,
            self.recall() * 100.0,
            self.tokens_per_correct()
                .map_or("-".to_string(), |v| format!("{v:.1}")),
        )
    }

    /// The row for `results.csv`.
    fn csv(&self, lookup: &str, symbol: &str, variant: &str) -> String {
        format!(
            "{lookup},{symbol},{variant},{},{},{},{:.4},{:.4},{},{}",
            self.lines,
            self.bytes,
            self.tokens,
            self.precision(),
            self.recall(),
            self.tokens_per_correct()
                .map_or(String::new(), |v| format!("{v:.1}")),
            self.reference,
        )
    }
}

/// `part / whole`, or `0.0` when `whole` is zero.
fn ratio(part: usize, whole: usize) -> f64 {
    if whole == 0 {
        0.0
    } else {
        part as f64 / whole as f64
    }
}

/// Counts tokens with Anthropic's `count_tokens` endpoint, or estimates them from the byte length
/// when there is no key or `RACLI_BENCH_TOKENS=estimate` is set.
struct Counter {
    /// Whether the endpoint is used.
    endpoint: bool,
    /// The tokens that the endpoint adds to any message, subtracted from every count.
    overhead: u64,
    /// The endpoint's counts, keyed by the model and a hash of the text.
    cache: HashMap<String, u64>,
    /// The file that keeps `cache` between runs.
    cache_path: PathBuf,
}

impl Counter {
    /// Loads the cache from `cache_path` and, if the endpoint is used, measures its message overhead.
    fn new(cache_path: &Path) -> Self {
        let estimate_only = std::env::var(TOKENS_VAR).is_ok_and(|v| v == "estimate");
        let endpoint = !estimate_only && std::env::var_os(API_KEY_VAR).is_some();
        let cache = fs::read_to_string(cache_path)
            .ok()
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_default();
        let mut counter = Counter {
            endpoint,
            overhead: 0,
            cache,
            cache_path: cache_path.to_path_buf(),
        };
        if endpoint {
            counter.overhead = counter.endpoint_count("a") - 1;
        }
        counter
    }

    /// A one-line description of the counter for the top of the report.
    fn label(&self) -> String {
        if self.endpoint {
            format!(
                "Anthropic `count_tokens` endpoint, model `{MODEL}`, message overhead of {} tokens subtracted",
                self.overhead
            )
        } else {
            "ESTIMATE: bytes / 2, not Claude token counts".to_string()
        }
    }

    /// The tokens in `text`, without the message overhead.
    fn count(&mut self, text: &str) -> u64 {
        if text.trim().is_empty() {
            0
        } else if self.endpoint {
            self.endpoint_count(text).saturating_sub(self.overhead)
        } else {
            (text.len() as u64).div_ceil(2)
        }
    }

    /// The endpoint's count for `text` as one user message, from the cache when it has it.
    fn endpoint_count(&mut self, text: &str) -> u64 {
        let mut hasher = DefaultHasher::new();
        text.hash(&mut hasher);
        let key = format!("{MODEL}:{:016x}", hasher.finish());
        if let Some(&tokens) = self.cache.get(&key) {
            return tokens;
        }
        let tokens = request_count(text);
        self.cache.insert(key, tokens);
        tokens
    }

    /// Writes the cache to its file.
    fn save(&self) {
        let text = serde_json::to_string_pretty(&self.cache).expect("serialize the token cache");
        fs::write(&self.cache_path, text).expect("write the token cache");
    }
}

/// Asks the `count_tokens` endpoint for the size of `text` as one user message. It runs `curl`
/// and passes the key through the environment, so the key never appears in the arguments.
fn request_count(text: &str) -> u64 {
    let body = json!({"model": MODEL, "messages": [{"role": "user", "content": text}]}).to_string();
    let mut child = Command::new("curl")
        .args(["-sS", "https://api.anthropic.com/v1/messages/count_tokens"])
        .args(["--variable", &format!("%{API_KEY_VAR}")])
        .args([
            "--expand-header",
            &format!("x-api-key: {{{{{API_KEY_VAR}}}}}"),
        ])
        .args(["-H", "anthropic-version: 2023-06-01"])
        .args(["-H", "content-type: application/json"])
        .args(["--data-binary", "@-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("spawn curl");
    child
        .stdin
        .take()
        .expect("curl stdin")
        .write_all(body.as_bytes())
        .expect("send the request body to curl");
    let output = child.wait_with_output().expect("wait for curl");
    let response: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap_or_else(|e| {
        panic!(
            "count_tokens gave no JSON ({e}): {}",
            String::from_utf8_lossy(&output.stdout)
        )
    });
    response["input_tokens"].as_u64().unwrap_or_else(|| {
        let start: String = text.chars().take(80).collect();
        panic!("count_tokens failed for the text {start:?}: {response}")
    })
}

/// Runs `script` with `bash -c` in `workspace` and returns its standard output.
fn run_shell(workspace: &Path, script: &str) -> String {
    let output = Command::new("bash")
        .arg("-c")
        .arg(script)
        .current_dir(workspace)
        .stderr(Stdio::null())
        .output()
        .expect("spawn bash");
    String::from_utf8_lossy(&output.stdout).into_owned()
}

/// What one racli call printed.
struct RacliOutput {
    /// The standard output, as the benchmark counts it.
    text: String,
    /// Whether racli warned on standard error that `--symbol-search-limit` cut the results.
    cut_by_limit: bool,
}

/// Runs `racli` with `args` against `socket` in the workspace and returns its standard output,
/// without the lines that the configuration says to drop.
fn run_racli(config: &Config, racli: &Path, socket: &Path, args: &[String]) -> RacliOutput {
    let output = Command::new(racli)
        .args(args)
        .env("RACLI_UNIX_SOCKET", socket)
        .current_dir(config.workspace_dir())
        .stderr(Stdio::piped())
        .output()
        .expect("spawn racli");
    let text = String::from_utf8_lossy(&output.stdout);
    let lines: Vec<&str> = text
        .lines()
        .filter(|line| !config.is_ignored(line))
        .collect();
    let text = if args.iter().any(|arg| arg == "--hover") {
        lines.iter().map(|line| format!("{line}\n")).collect()
    } else {
        sort_under_headers(&lines)
    };
    RacliOutput {
        text,
        cut_by_limit: String::from_utf8_lossy(&output.stderr).contains(LIMIT_WARNING),
    }
}

/// Joins `lines` with the indented lines under each header sorted. rust-analyzer lists them in a
/// different order each time the server starts, and a fixed order keeps the output, and so its
/// count, the same from run to run.
fn sort_under_headers(lines: &[&str]) -> String {
    lines
        .chunk_by(|_, line| line.starts_with(char::is_whitespace))
        .flat_map(|chunk| {
            let (header, rest) = chunk.split_first().expect("a chunk is never empty");
            let mut rest = rest.to_vec();
            rest.sort_unstable();
            std::iter::once(*header).chain(rest)
        })
        .map(|line| format!("{line}\n"))
        .collect()
}

/// Reads a leading `path:line` from a grep-style line (`path:line:text`, or a bare `path:line`).
fn leading_location(line: &str) -> Option<Location> {
    let (path, rest) = line.split_once(':')?;
    if path.is_empty() || path.contains(char::is_whitespace) {
        return None;
    }
    let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
    let tail = &rest[digits.len()..];
    if digits.is_empty() || !(tail.is_empty() || tail.starts_with(':')) {
        return None;
    }
    Some((path.to_string(), digits.parse().ok()?))
}

/// Reads the `path:line:col` at the end of a racli line, such as `Name [KIND] path:line:col`.
fn trailing_location(line: &str) -> Option<Location> {
    let token = line.split_whitespace().last()?;
    let mut parts = token.rsplitn(3, ':');
    parts.next()?.parse::<u32>().ok()?;
    let line = parts.next()?.parse().ok()?;
    Some((parts.next()?.to_string(), line))
}

/// The locations that racli's output gives, which are the reference for scoring every variant.
fn racli_answers(lookup: Lookup, output: &str) -> Vec<Location> {
    output
        .lines()
        .filter_map(|line| {
            let indented = line.starts_with(char::is_whitespace);
            match (lookup, indented) {
                (Lookup::Definition | Lookup::References, false) => trailing_location(line),
                (Lookup::References | Lookup::Implementations, true) => {
                    leading_location(line.trim_start())
                }
                (Lookup::CallHierarchy, true) => trailing_location(line),
                _ => None,
            }
        })
        .collect()
}

/// The line of the nearest `fn` at or above `location`, mirroring `grep -nE '(^|[[:space:]])fn[[:space:]]'`.
fn enclosing_fn(workspace: &Path, location: &Location) -> Option<Location> {
    let text = fs::read_to_string(workspace.join(&location.0)).ok()?;
    let lines: Vec<&str> = text.lines().take(location.1 as usize).collect();
    let index = lines
        .iter()
        .rposition(|line| line.split_whitespace().any(|word| word == "fn"))?;
    Some((location.0.clone(), index as u32 + 1))
}

/// The locations that a variant's output gives. A location that has no function is
/// `("", 0)`, which is never in the reference set.
fn answers(lookup: Lookup, variant: &Variant, output: &str, workspace: &Path) -> Vec<Location> {
    if variant.is_racli() {
        return racli_answers(lookup, output);
    }
    output
        .lines()
        .filter_map(leading_location)
        .map(|location| {
            if variant.by_function {
                enclosing_fn(workspace, &location).unwrap_or_default()
            } else {
                location
            }
        })
        .collect()
}

/// The reference locations for one query, and whether racli warned that the symbol search limit cut them.
struct Reference {
    /// The locations that racli gives.
    locations: BTreeSet<Location>,
    /// Whether the last racli call warned about `--symbol-search-limit`.
    cut_by_limit: bool,
}

/// Asks racli (with `args`) until two answers in a row are the same and not empty, so the index
/// is complete, and returns the locations. It panics if that takes longer than the configured
/// time, because an empty reference set would make every score meaningless (a stale query, or a
/// symbol that is gone).
fn settled_reference(
    config: &Config,
    lookup: Lookup,
    args: &[String],
    racli: &Path,
    socket: &Path,
) -> Reference {
    let deadline = Instant::now() + Duration::from_secs(config.settle_timeout_secs);
    let mut previous = None;
    loop {
        let output = run_racli(config, racli, socket, args);
        let locations: BTreeSet<Location> =
            racli_answers(lookup, &output.text).into_iter().collect();
        if !locations.is_empty() && previous.as_ref() == Some(&output.text) {
            return Reference {
                locations,
                cut_by_limit: output.cut_by_limit,
            };
        }
        assert!(
            Instant::now() < deadline,
            "`racli {}` gave no stable locations: is the query stale or the index incomplete?",
            args.join(" ")
        );
        previous = Some(output.text);
        thread::sleep(Duration::from_secs(1));
    }
}

/// The source line at `location`, cut to 100 characters, for `grep-only.txt`.
fn source_line(workspace: &Path, location: &Location) -> String {
    fs::read_to_string(workspace.join(&location.0))
        .ok()
        .and_then(|text| {
            let index = (location.1 as usize).checked_sub(1)?;
            let line = text.lines().nth(index)?;
            Some(line.trim().chars().take(100).collect())
        })
        .unwrap_or_default()
}

/// Reads the configuration file: the one that `RACLI_BENCH_CONFIG` names, or the default.
fn load_config() -> (PathBuf, Config) {
    let path = std::env::var_os(CONFIG_VAR)
        .map(PathBuf::from)
        .unwrap_or_else(|| Path::new(env!("CARGO_MANIFEST_DIR")).join(DEFAULT_CONFIG));
    let text = fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("cannot read the configuration {}: {e}", path.display()));
    let config = serde_yaml_ng::from_str(&text)
        .unwrap_or_else(|e| panic!("invalid configuration {}: {e}", path.display()));
    (path, config)
}

#[cfg(not(unix))]
fn main() {
    eprintln!("bench `tokens` requires Unix (racli uses Unix domain sockets)");
}

#[cfg(unix)]
fn main() {
    let (config_path, config) = load_config();
    let lookups: Vec<(Lookup, &LookupConfig)> = config
        .lookups
        .iter()
        .map(|lookup| {
            let kind = Lookup::from_name(&lookup.name).unwrap_or_else(|| {
                panic!(
                    "{}: unknown lookup `{}` (use find-definition, find-references, find-implementations or call-hierarchy)",
                    config_path.display(),
                    lookup.name
                )
            });
            assert!(
                !lookup.tools.racli.is_empty(),
                "{}: lookup `{}` needs a racli command, which is the reference",
                config_path.display(),
                lookup.name
            );
            (kind, lookup)
        })
        .collect();
    let workspace = config.workspace_dir();
    if !workspace.join("Cargo.toml").is_file() {
        eprintln!(
            "skipping `tokens`: no Rust workspace at {} (from {})",
            workspace.display(),
            config_path.display()
        );
        return;
    }
    for var in &config.unset_env {
        // SAFETY: nothing else runs yet, so no other thread reads the environment.
        unsafe { std::env::remove_var(var) };
    }

    let racli = support::racli_executable();
    let server = support::RacliServer::start(&workspace, |sock| {
        support::poll_until(
            Duration::from_secs(config.ready_timeout_secs),
            || async {
                matches!(
                    tokio::time::timeout(
                        Duration::from_secs(10),
                        racli::client::Client::new(sock)
                            .unwrap()
                            .search(&config.ready_symbol)
                    )
                    .await,
                    Ok(Ok(_))
                )
            },
            "racli server did not accept search RPCs in time",
        );
    });
    let socket = server.socket().to_path_buf();
    let out_dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("target")
        .join(&config.name);
    fs::create_dir_all(&out_dir).expect("create the results directory");
    let mut counter = Counter::new(&out_dir.join("token-cache.json"));

    let mut csv = vec![
        "lookup,symbol,variant,lines,bytes,tokens,precision,recall,tokens_per_correct,reference_locations"
            .to_string(),
    ];
    let mut grep_only = Vec::new();
    let mut grep_missed = Vec::new();
    println!("Config: {}", config_path.display());
    println!("Workspace: {}", workspace.display());
    println!(
        "{}",
        run_racli(&config, &racli, &socket, &["version".to_string()]).text
    );
    println!("Counter: {}\n", counter.label());
    for (kind, lookup) in lookups {
        println!("### {}\n\n{TABLE_HEADER}", lookup.name);
        let mut sums: Vec<(String, Totals)> = Vec::new();
        let mut cut = Vec::new();
        for symbol in &lookup.symbols {
            let reference_args = racli_args(&lookup.tools.racli[0].args, symbol);
            let reference = settled_reference(&config, kind, &reference_args, &racli, &socket);
            if reference.cut_by_limit {
                cut.push(symbol.as_str());
            }
            let reference = reference.locations;
            for variant in variants(lookup, symbol, &config) {
                let output = match &variant.run {
                    Run::Shell(script) => run_shell(&workspace, script),
                    Run::Racli(args) => run_racli(&config, &racli, &socket, args).text,
                };
                let given = answers(kind, &variant, &output, &workspace);
                let found: BTreeSet<&Location> =
                    given.iter().filter(|l| reference.contains(*l)).collect();
                let row = Totals {
                    lines: output.lines().filter(|l| !l.trim().is_empty()).count(),
                    bytes: output.len(),
                    tokens: counter.count(&output),
                    answers: given.len(),
                    correct_answers: given.iter().filter(|l| reference.contains(*l)).count(),
                    found: found.len(),
                    reference: reference.len(),
                };
                println!("{}", row.markdown(symbol, &variant.name));
                csv.push(row.csv(&lookup.name, symbol, &variant.name));
                if !variant.is_racli() {
                    let extra: BTreeSet<&Location> =
                        given.iter().filter(|l| !reference.contains(*l)).collect();
                    for location in extra {
                        grep_only.push(format!(
                            "{} {symbol} [{}] {}:{}: {}",
                            lookup.name,
                            variant.name,
                            location.0,
                            location.1,
                            source_line(&workspace, location)
                        ));
                    }
                    let given: BTreeSet<&Location> = given.iter().collect();
                    for location in reference.iter().filter(|l| !given.contains(l)) {
                        grep_missed.push(format!(
                            "{} {symbol} [{}] {}:{}: {}",
                            lookup.name,
                            variant.name,
                            location.0,
                            location.1,
                            source_line(&workspace, location)
                        ));
                    }
                }
                match sums.iter_mut().find(|(name, _)| *name == variant.name) {
                    Some((_, total)) => total.add(&row),
                    None => {
                        let mut total = Totals::default();
                        total.add(&row);
                        sums.push((variant.name.clone(), total));
                    }
                }
            }
        }
        for (name, total) in &sums {
            println!("{}", total.markdown("**Total**", name));
        }
        if !cut.is_empty() {
            println!(
                "\nNote: racli warned that --symbol-search-limit cut the results for: {}.",
                cut.join(", ")
            );
        }
        println!();
        // Keep the counts if a later lookup fails: an endpoint call for each output costs time.
        counter.save();
    }

    fs::write(out_dir.join("results.csv"), csv.join("\n") + "\n").expect("write results.csv");
    fs::write(out_dir.join("grep-only.txt"), grep_only.join("\n") + "\n")
        .expect("write grep-only.txt");
    fs::write(
        out_dir.join("grep-missed.txt"),
        grep_missed.join("\n") + "\n",
    )
    .expect("write grep-missed.txt");
    counter.save();
    println!(
        "Wrote results.csv, grep-only.txt, grep-missed.txt and token-cache.json to {}",
        out_dir.display()
    );
}
