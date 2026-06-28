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

Rules match over two layers, never over raw text. A frontend produces them from a
single parse of each file.

**Structural layer.** Normalized constructs projected from the parse, as a flat
`Vec<Event>`. An `Event` is:

```rust
struct Event {
    kind: EventKind,              // Import, Call, TypeDecl, EnvAccess, ScopeStart, …
    language: Language,
    file: Arc<Utf8PathBuf>,       // shared by every event of a file
    span: Span,                   // 1-based line/col, end-exclusive; 0-based byte offsets
    attrs: BTreeMap<String, serde_json::Value>,
}
```

The `EventKind` vocabulary is fixed: `File`, `PackageAdded`, `Import`, `Export`,
`Call`, `NewExpr`, `TypeDecl`, `FunctionDecl`, `MethodDecl`, `ClassDecl`,
`Attribute`, `EnvAccess`, `StringLiteral`, `Comment`, `ScopeStart`, `ScopeEnd`,
`GraphqlOperation`, `GeneratedFile`, `ConfigFile`, and `Token`. These names are
the `Kind` you write in a rule. A rule against this layer is language-independent:
it applies to every language with a frontend.

Every predicate reads an attribute and normalizes its JSON value to a list of
strings before testing: a scalar becomes one string, an array becomes its
elements, objects and null become nothing. So `symbols ~ /^use/` tests each name
in the `symbols` array independently.

**Token layer.** Every node of the parse, as a flat `Vec<Token>`. A `Token` is a
`Copy` struct with no owned strings — node kind, text, and enclosing-function name
are interned to `u32` ids (`Sym`) in a per-file `Interner`:

```rust
struct Token {
    kind: Sym, text: Sym, func: Sym,        // interned strings
    named: bool,                            // named node vs. bare symbol (`switch_statement` vs `switch`)
    start_byte: u32, end_byte: u32,
    start_line: u32, start_col: u32, end_line: u32, end_col: u32,
    curly: u16, round: u16, bracket: u16,   // nesting depths at this token
    jmp: u32,                               // index of the matching delimiter, or NO_JMP (u32::MAX)
}
```

The stream is one contiguous allocation; previous and next token are index ±1, and
`jmp` links each `(`/`{`/`[` to its partner (paired in one stack pass over the
stream). The token layer is built only when a loaded rule references the `Token`
kind — single-event rules that never touch it pay nothing.

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
predicate (all ANDed). Operators:

| Syntax                      | Holds when                                                  |
| --------------------------- | ----------------------------------------------------------- |
| `attr = "v"` / `attr = 5`   | some string form of the attribute equals `v`                |
| `attr ~ /re/`               | some string form matches the regex                          |
| `attr !~ /re/`              | no string form matches the regex                            |
| `attr in ["a", "b"]`        | some string form is in the set                              |
| `attr > 5` (`< >= <=`)      | some string form parses as a number and compares true       |
| `attr == $v` / `attr != $v` | equals / differs from a captured value (see Unification)    |

### Scoping

A rule applies to all files by default. `lang` restricts it by language; `in` and
`not in` filter by glob. Excludes are checked first and win over includes:

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

Calls can be written with positional arguments instead of named attributes. The
sugar desugars to ordinary predicates on `name`, `receiver`, `arg0…argN`,
`argN_kind`, and `arg_count`:

```
log(_, $level: string, ..)          # name="log", arg1 captured, arg1_kind="string", arg_count>=2
db.query($sql: string)              # name="query", receiver="db", arg0 captured, arg0_kind="string", arg_count=1
```

- The receiver is the object before the method. `obj.method(…)` adds
  `receiver = "obj"`; `$obj.method(…)` captures or constrains it.
- Each argument is `_` (no predicate), a literal, or a `$capture`, each with an
  optional `: kind`. The kind is the argument's syntactic form (`string`,
  `number`, `bool`, `identifier`, `member`, `call`, `object`, `array`, `function`,
  `regex`, `template`, `null`, `undefined`, `other`) — how it is written, taken
  from the parse node type, not an inferred static type.
- A trailing `..` sets `arg_count >= N` instead of exact arity. Without it, the
  arity is exact (`arg_count = N`).

The frontend records the first 32 arguments per call (`arg_count` still reflects
the true total).

### Captures and unification

A `$name` binds on its first use and constrains on every later use, so one rule
correlates two positions by the same value. Binding is tracked per rule: the first
`$obj` in a call-sugar pattern (or an `as v = attr` clause) records that the
variable captures that attribute; a later `$obj` compiles to an `== ` backreference
against it. Inside an explicit `Kind[…]`, `attr == $v` and `attr != $v` are always
backreferences — the variable must have been bound earlier in the rule.

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

Mechanics. The steps run against a region of events: the whole file, or — with
`in scope` — the interior of each `{}` block (paired by scope id). Matching is
tried from every start index in the region, and from that index the steps must
consume the rest of the region exactly to its end. So a trailing `not … *` means
"and nothing in the remainder of the scope matches." Each start index that
succeeds reports its first event, deduplicated by byte offset (an event inside
nested scopes is reported once per rule).

A step may carry:

- a quantifier — `?` (zero or one), `*` (zero or more), `+` (one or more);
- `not` — the step's match is inverted, so `not X *` consumes a run of non-`X`;
- alternation — `( A | B )`, where each arm is one matcher;
- `any` — the wildcard kind;
- `as v = attr, …` — capture one or more attributes, in addition to `$…`.

Backreferences (`$ev` reused, or `attr == $v`) compare against the first string
form captured into the variable, so `$obj … $obj` keys on the same object.

### Scope predicates

To ask about the block a construct sits in without writing a full sequence:

```
rule LOCK_WITHOUT_UNLOCK (error) {
  match Call[name ~ /acquire$/]
  where scope not contains Call[name ~ /release$/]
  message "A lock acquired here is never released in the same scope."
}
```

The matched event's enclosing scope is the innermost `ScopeStart`/`ScopeEnd` pair
whose byte range contains it (the whole file if none). Clauses: `where scope
contains …`, `where scope not contains …`, and `where scope followed by …`
(matches an event after this one in the same block). Multiple clauses on one rule
are ANDed.

### Composition and counting

A rule can be derived from other rules in a pass that runs after all single and
sequence rules have produced their violations:

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

`compose` groups each referenced rule's violations by a key tuple (`by file,
function`; default `file`). `intersection` keeps keys present in every referenced
rule, `union` keeps keys present in any, `difference` keeps keys in the first rule
and none of the rest; it emits one violation per surviving key. `count` groups one
rule's violations per `file` or per `function` and emits when the group size
compares true (`>`, `<`, `>=`, `<=`, `==`) against the threshold. Aggregates read
only the primary violations, never each other.

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

Token fields: `node_kind`, `text`, `function` (enclosing function), `named`,
`curly_depth` / `round_depth` / `bracket_depth`, `range_lines` (end_line −
start_line + 1), and `text_len` (byte length). `codepolicy events <file> --tokens`
lists a file's node kinds. Token rules match a single construct: ordering,
correlation, and backreferences live in the structural layer (a token rule has no
binding environment, so `==`/`!=` never match it).

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
`#` starts a comment. A rule has exactly one body: `match`, `sequence`, `compose`,
or `count`; `where scope` attaches only to a `match`.

Every rule has an equivalent YAML form, and `check` reads either format; both
compile to the same representation. `codepolicy init` writes a starter pack as
`--format rules` or `--format yaml`.

## Escape hatches

Two repo-level mechanisms complement the inline `unless` guards:

- **Waivers** — YAML files under `.codepolicy/waivers/`, each with a `rule` and a
  `file` key. A waiver suppresses that rule in that file unconditionally,
  independent of whether the rule has an `unless`.
- **ADRs** — decision records under `docs/adr/`, each with `topic` and `status`. A
  record whose status is `accepted` (case-insensitive) satisfies any `unless adr
  "topic"` guard with that topic.

A candidate violation is dropped if a waiver names its `(rule, file)`, or its
rule's `unless` matches. Both directories are configurable (`waivers`/`adrs`
directives, or `waivers_dir`/`adr_dir` in YAML).

## How a check runs

`Project::check` is the pipeline:

1. **Discover.** Walk the root with the `ignore` crate, honoring `.gitignore`,
   keeping only UTF-8 paths a frontend claims, sorted for determinism.
2. **Extract.** Process files in parallel (`rayon`). Each frontend returns
   `(Vec<Event>, Option<TokenStream>)`. The token stream is requested only if some
   loaded rule references the `Token` kind. A file that fails to parse prints a
   warning and contributes nothing; the run continues.
3. **Index.** `EventIndex` builds two maps over the combined events: `by_kind`
   (each kind's events in encounter order) and `by_file` (each file's events
   sorted by start byte, for sequence and scope matching).
4. **Match.** Two passes. Pass one runs every non-aggregate rule: a `Token` rule
   scans the token streams, a sequence rule runs the matcher over each file or
   scope region, a single rule scans `by_kind[event]` and, if present, evaluates
   its `where scope`. Pass two runs `compose`/`count` over a snapshot of pass-one
   violations. A violation is emitted only after passing the suppression gate
   (waiver or `unless`).
5. **Sort.** Violations are ordered by file, line, column, then rule id.

Single-event rules scan only the events of their kind, not every event, so cost
scales with matches rather than rules × events.

## Running it

```bash
codepolicy init                            # write a starter rule pack
codepolicy check                           # check the current repo
codepolicy check services/ --format agent  # output for a subtree
codepolicy check --rules my.rules --format json
codepolicy events path/to/File             # dump the structural events for a file
codepolicy events path/to/File --tokens    # add the resolved token layer
codepolicy explain-rule NO_DEBUGGER        # show how a rule is interpreted
```

`check` discovers a `codepolicy.rules` or `codepolicy.yaml` at the root, or takes
`--rules <file>`. Exit codes: `1` if any error-level violation is found, `0` if
clean, `2` on a usage or I/O error. The three formats:

- **human** — per violation: `SEVERITY rule_id`, `file:line:col`, the description,
  a `Matched:` line summarizing the construct, and a `Remediation:` line; a footer
  counts errors and warnings.
- **json** — `{ "violations": [...], "summary": { "errors", "warnings", "total" } }`,
  each violation carrying `rule_id`, `severity`, `file`, `span`, and the matched
  construct.
- **agent** — terse, compiler-like: `SEVERITY rule_id at file:line:col` followed by
  `matched:`, `why:`, and `fix:` lines, ending with an instruction to fix errors
  before committing.

## Languages

The grammar, the matcher, and both layers are language-independent. A language is
supported by a frontend that turns a file into the token layer, the structural
layer, or both. Frontends ship for TypeScript / JavaScript (tree-sitter) and for
package manifests; a rule against the structural layer applies to a language the
moment its frontend exists.

### How the TypeScript/JavaScript frontend works

One tree-sitter parse, one recursive walk. At each node the walk: emits a
structural event if the node maps to one (`import_statement` → `Import` with
`source` and `symbols`; `call_expression` → `Call` with `name`, `callee`,
`receiver`, `string_args`, `arg_count`, and per-position `argN`/`argN_kind`;
`interface`/`type_alias` → `TypeDecl`; function declarations → `FunctionDecl`;
`process.env.X` and `process.env["X"]` → `EnvAccess`; `comment` → `Comment`;
`string` → `StringLiteral`); appends a `Token` for the node when tokens are
requested; and, for `statement_block`/`class_body`, emits paired
`ScopeStart`/`ScopeEnd` events with a per-file scope id. A `Frame` carried down the
walk tracks nesting depth (`statement_block`/`class_body` raise curly depth;
argument and parameter lists and parenthesized expressions raise round depth;
arrays raise bracket depth) and the enclosing function name; these become the
`curly_depth`/`round_depth`/`bracket_depth`/`range_lines`/`text_len`/`function`
attributes on every event. After the walk, delimiter tokens are paired into `jmp`
links with a stack. The manifest frontend instead reads `package.json` and emits a
`PackageAdded` event per dependency across the four dependency sections; it
produces no tokens.

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

A frontend can produce the token layer alone (a lexer suffices), the structural
layer alone (as the manifest frontend does), or both. Implement the trait in a
module under `codepolicy-frontends/src/`, register it in `default_frontends()`, and
add a fixture and assertion in `crates/codepolicy-cli/tests/fixtures.rs`. Follow
the conventions: 1-based, end-exclusive spans; set `language` on everything
emitted; reuse the attribute names above so existing rules apply unchanged.

## Architecture

A Cargo workspace:

| Crate                  | Responsibility                                                       |
| ---------------------- | ------------------------------------------------------------------- |
| `codepolicy-events`    | The structural model (`Event` / `EventKind` / `Span` / `Language`) and the token layer (`Token` / `TokenStream` / `Interner`) |
| `codepolicy-frontends` | `LanguageFrontend` and the shipped frontends (tree-sitter TS/JS, manifest) |
| `codepolicy-rules`     | Rule grammar (textual DSL + YAML), compilation to `CompiledRule`, the attribute predicate language |
| `codepolicy-match`     | `EventIndex` and matching: single construct, sequences, scope predicates, compose/count, token rules, waiver/ADR/`unless` evaluation |
| `codepolicy-report`    | `human` / `json` / `agent` rendering                                |
| `codepolicy-core`      | `Project`: discovery, parallel extraction, the check pipeline       |
| `codepolicy-cli`       | The `codepolicy` binary, the bundled starter packs, fixtures        |

## Build and test

```bash
cargo build --release         # binary at target/release/codepolicy
cargo test                    # unit + end-to-end fixture tests
cargo clippy --all-targets
```

Design rationale: [`../proposal.md`](../proposal.md).
