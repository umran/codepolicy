# codepolicy

codepolicy checks a codebase against rules you define and reports each violation
with its file and line. A rule is a structural constraint over code — for
example, an import allowed only in certain paths, a call that must be paired with
another in the same scope, or a banned construct. The tool has no built-in notion
of correct code; it enforces the rules you write. It exits non-zero on error-level
violations, so it works as a local check, a CI gate, or an agent feedback loop.

The approach follows NASA/JPL's Cobra: pattern rules over a flat stream of code
facts, each match carrying file and line evidence.

## How it sees code

Rules match over two layers rather than raw text:

- **Token layer** — every lexical and syntactic construct, matchable by its
  native kind (a `switch`, a decorator, a ternary, an operator, a keyword). Each
  token records its nesting depth, its enclosing function, and the index of its
  matching delimiter. The stream is flat and compact.
- **Structural layer** — normalized constructs projected from the parse: imports,
  calls (with receiver and positional arguments), class / function / type
  declarations, environment access, string and comment literals, package-manifest
  entries, and scope boundaries. These are language-independent: one rule written
  against this layer applies to every language that has a frontend.

Use the structural layer for the concepts it names. Use the token layer for
anything it does not.

## Writing rules

Rules are written in a textual grammar. A rule names what to match, optionally
restricts it by language and path, and attaches a message. The basic form is a
single construct with attribute predicates:

```
rule NO_RAW_ENDPOINT_FETCH (error) {
  match Call[name = "fetch", string_args ~ /\/graphql/]
  message "Don't call the GraphQL endpoint directly — use the generated client."
}
```

`Kind[ … ]` matches a construct of that kind whose attributes satisfy every
predicate. Operators:

| Syntax                      | Meaning                                |
| --------------------------- | -------------------------------------- |
| `attr = "v"` / `attr = 5`   | equality                               |
| `attr ~ /re/`               | matches a regex                        |
| `attr !~ /re/`              | does not match a regex                 |
| `attr in ["a", "b"]`        | is one of a set                        |
| `attr > 5` (`< >= <=`)      | numeric comparison                     |
| `attr == $v` / `attr != $v` | equals / differs from a captured value |

### Scoping

A rule applies to all files by default. `lang` restricts it by language; `in` and
`not in` include and exclude files by glob:

```
rule NO_PROVIDER_SDK_OUTSIDE_INFRA (error) {
  lang typescript, javascript, go, python
  in  "services/**", "apps/**"
  not in "**/infrastructure/**"
  match Import[source ~ /^(aws-sdk|@aws-sdk\/.*|stripe|twilio|googleapis)$/]
  message "Use provider SDKs only from the approved infrastructure layer."
}
```

### Calls, arguments, receivers

Calls can be written with positional arguments instead of named attributes:

```
log(_, $level: string, ..)          # bind the 2nd arg; it must be a string literal
db.query($sql: string)              # a one-argument method call on any `db`
$client.send($msg, ..)              # capture the receiver and the first argument
```

- The receiver is the object before the method. `obj.method(…)` matches a literal
  receiver; `$obj.method(…)` captures or constrains it.
- Each argument is `_` (any), a literal, or a `$capture`, each with an optional
  `: kind`. The kind is the argument's syntactic form (`string`, `number`,
  `bool`, `identifier`, `member`, `call`, `object`, `array`, `function`, `regex`,
  `template`) — how it is written, not an inferred static type.
- A trailing `..` allows further arguments; without it the arity is exact.

### Captures and unification

A `$name` binds on first use and constrains on every later use. This correlates
two positions by the same value — the same object, key, or identifier — without
an explicit equality predicate.

### Sequences

A sequence matches an ordered run of constructs, optionally confined to one
enclosing scope. With capture, this expresses acquire-without-release rules: the
violation is the acquire with no matching release in the same scope.

```
rule LISTENER_LEAK (warning) {
  sequence in scope {
    $obj.addEventListener($ev, ..)
    not $obj.removeEventListener($ev, ..) *
  }
  message "addEventListener has no matching removeEventListener (same object + event)."
}
```

A step may carry:

- a quantifier — `?` (optional), `*` (zero or more), `+` (one or more);
- `not` — the step must not appear;
- alternation — `( A | B )`;
- `any` — the wildcard kind;
- `as v = attr, …` — explicit captures, in addition to `$…`.

`in scope` confines the sequence to one lexical block. The matcher tries every
start position and reports every occurrence.

### Scope predicates

To ask about the block a construct sits in without writing a full sequence:

```
rule LOCK_WITHOUT_UNLOCK (error) {
  match Call[name ~ /acquire$/]
  where scope not contains Call[name ~ /release$/]
  message "A lock acquired here is never released in the same scope."
}
```

Clauses: `where scope contains …`, `where scope not contains …`, and `where scope
followed by …` (appears later in the same block).

### Composition and counting

A rule can be derived from other rules in a final pass — set algebra over their
results, or a cardinality threshold:

```
rule HOTSPOT (warning) {
  compose intersection of TOUCHES_AUTH, TOUCHES_BILLING by file, function
  message "A function that reaches both auth and billing."
}

rule TOO_MANY_ENV_READS (warning) {
  count RAW_ENV_ACCESS per file > 10
  message "This file reads the environment directly more than ten times."
}
```

`compose` takes `intersection`, `union`, or `difference`, keyed by attributes
(`by file, function`). `count` compares per `file` or per `function` with `>`,
`<`, `>=`, `<=`, or `==`.

### Token layer

Constructs the structural layer does not name are matched by native `node_kind`:

```
rule NO_DEBUGGER (error) {
  match Token[node_kind = "debugger"]
  message "Remove debugger statements."
}

rule NO_TOP_LEVEL_SWITCH (warning) {
  match Token[node_kind = "switch_statement", curly_depth = 0]
  message "Replace top-level switch with a lookup table."
}
```

Token predicates: `node_kind`, `text`, `function` (enclosing function), `named`,
`curly_depth` / `round_depth` / `bracket_depth`, `range_lines`, `text_len`.
`codepolicy events <file> --tokens` lists a file's `node_kind`s. Token rules match
a single construct; ordering and correlation use the structural layer.

### Inline exceptions

A rule can exempt cases without being disabled:

```
rule NO_UNAPPROVED_STATE_LIBRARY (error) {
  match PackageAdded[name in ["redux", "recoil", "mobx", "xstate"]]
  unless adr "state management exception"
  message "Pick a state library through an architecture decision record."
}
```

`unless path "glob"` exempts matching files; `unless adr "topic"` defers to an
accepted decision record; `unless waiver` honors a file-scoped waiver.

### Other clauses

`(error)` / `(warning)` after the rule name sets severity: errors fail a run,
warnings do not. `message "…"` is shown on a hit; `desc "…"` documents the rule;
`#` starts a comment.

Every rule has an equivalent YAML form, and `check` reads either format.
`codepolicy init` writes a starter pack as `--format rules` or `--format yaml`.

## Escape hatches

Two repo-level mechanisms complement the inline `unless` guards:

- **Waivers** — YAML files under `.codepolicy/waivers/`, each naming a `rule` and
  a `file`. A waiver suppresses that rule in that file regardless of whether the
  rule opted in.
- **ADRs** — decision records under `docs/adr/`, each with a `topic` and a
  `status`. An accepted ADR satisfies any `unless adr "topic"` guard repo-wide.

Both directories are configurable.

## Running it

```bash
codepolicy init                            # write a starter rule pack
codepolicy check                           # check the current repo
codepolicy check services/ --format agent  # output for a subtree
codepolicy check --rules my.rules --format json
codepolicy events path/to/File             # dump the structural constructs for a file
codepolicy events path/to/File --tokens    # dump the token layer
codepolicy explain-rule NO_DEBUGGER        # show how a rule is interpreted
```

`check` discovers a `codepolicy.rules` or `codepolicy.yaml` at the root, or takes
`--rules <file>`. It scans supported files in parallel and prints violations in
`human`, `json`, or `agent` format. Exit codes: `1` if any error-level violation
is found, `0` if clean, `2` on a usage or I/O error.

## Languages

The grammar, the matcher, and both layers are language-independent. Language
support is a frontend: it turns a file into the token layer, the structural
layer, or both. Frontends ship for TypeScript / JavaScript (tree-sitter) and for
package manifests. Adding a language is one trait implementation, after which any
rule written against the structural layer applies to it.

### Adding a frontend

The contract is one trait:

```rust
pub struct SourceFile<'a> { pub path: Utf8PathBuf, pub text: &'a str }

#[derive(Default)]
pub struct Extracted {
    pub tokens: Option<TokenStream>, // the token layer
    pub events: Vec<Event>,          // the structural layer (optional)
}

pub trait LanguageFrontend: Sync {
    fn name(&self) -> &'static str;
    fn supports_file(&self, path: &Utf8Path) -> bool;
    fn extract(&self, file: &SourceFile<'_>, want_tokens: bool) -> anyhow::Result<Extracted>;
}
```

A frontend can produce the token layer alone (a lexer suffices; no parse tree
required), the structural layer alone (as the manifest frontend does), or both
from one parse (as the tree-sitter frontend does). The token layer is built only
when `want_tokens` is set, which the pipeline does only when a loaded rule
references `Token`.

To add one: implement the trait in a module under `codepolicy-frontends/src/`,
register it in `default_frontends()`, and add a fixture and assertion in
`crates/codepolicy-cli/tests/fixtures.rs`. Follow the shared conventions: 1-based,
end-exclusive spans; set `language` on everything emitted; reuse the attribute
vocabulary so existing rules apply unchanged (`Import` carries `source` and
`symbols`; `Call` carries `name`, `callee`, `receiver`, `string_args`,
`arg_count`, and per-position `argN` / `argN_kind`). The shipped frontends are the
references: `ts_js.rs` (parser, both layers) and `manifest.rs` (structured-only).

## Architecture

A Cargo workspace:

| Crate                  | Responsibility                                                       |
| ---------------------- | ------------------------------------------------------------------- |
| `codepolicy-events`    | The structural model (`Event` / `EventKind` / `Span` / `Language`) and the token layer (`Token` / `TokenStream` / `Interner`) |
| `codepolicy-frontends` | `LanguageFrontend` and the shipped frontends (tree-sitter TS/JS, manifest) |
| `codepolicy-rules`     | Rule grammar (textual DSL + YAML), compilation, the attribute predicate language |
| `codepolicy-match`     | Indexing and matching: single construct, sequences, scope predicates, compose/count, token rules, `unless`/waiver/ADR evaluation |
| `codepolicy-report`    | `human` / `json` / `agent` rendering                                |
| `codepolicy-core`      | File discovery, parallel extraction, the check pipeline             |
| `codepolicy-cli`       | The `codepolicy` binary, the bundled starter packs, fixtures        |

## Build and test

```bash
cargo build --release         # binary at target/release/codepolicy
cargo test                    # unit + end-to-end fixture tests
cargo clippy --all-targets
```

Design rationale: [`../proposal.md`](../proposal.md).
