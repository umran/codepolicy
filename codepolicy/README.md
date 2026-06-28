# codepolicy

Write your own rules about what code in a repository may and may not do — then
enforce them across the entire tree, fast, with precise evidence.

codepolicy ships no opinion about what "good code" is. It is a programmable
engine: *you* declare the constraints that matter for *your* codebase — an
architectural boundary that must not be crossed, a banned construct, an access
layer everything has to go through, a resource that must be released wherever
it's acquired, a dependency that needs sign-off — and codepolicy finds every
place a rule is broken and reports it as `file:line` with a remediation message.
The same rule pack works as a local pre-commit check, a CI gate, or a feedback
loop for an LLM coding agent.

It's inspired by NASA/JPL's Cobra analyzer: cheap pattern rules over a flat
stream of code facts, with concrete evidence for every hit — no bespoke
analyzer per rule.

## How it sees code

A rule never matches raw text (which can't tell a keyword from the same letters
inside a string or comment). Instead, each file is exposed through two layers,
and a rule can target either:

- **Token layer** — every lexical and syntactic construct is individually
  matchable by its native kind: a `switch`, a decorator, a ternary, an operator,
  a bare keyword. Each token carries its nesting depth, its enclosing function,
  and a link to its matching delimiter. It's a compact, flat stream built to run
  over millions of lines, and it reaches *anything* in the grammar of a
  language.

- **Structural layer** — normalized, cross-language constructs projected from
  the parse: imports, calls (with their receiver and positional arguments),
  class / function / type declarations, environment access, string and comment
  literals, package-manifest entries, lexical scope boundaries, and more. A rule
  written against this layer means the same thing regardless of which language
  the file is in.

The structural layer is what you reach for first — it's expressed in concepts,
not syntax. The token layer is the escape hatch beneath it: when you need a
construct the structural layer hasn't named, match it directly.

## Writing rules

Rules are written in a concise, Cobra-flavored grammar. A rule names a thing to
look for, optionally scopes it to certain languages and paths, and attaches a
message. The simplest shape is a single construct with attribute predicates:

```
rule NO_RAW_ENDPOINT_FETCH (error) {
  match Call[name = "fetch", string_args ~ /\/graphql/]
  message "Don't call the GraphQL endpoint directly — use the generated client."
}
```

`Kind[ … ]` matches a construct of that kind whose attributes satisfy every
predicate inside the brackets. The predicate operators:

| Syntax                      | Meaning                                   |
| --------------------------- | ----------------------------------------- |
| `attr = "v"` / `attr = 5`   | equality                                  |
| `attr ~ /re/`               | matches a regex                           |
| `attr !~ /re/`              | does **not** match a regex                |
| `attr in ["a", "b"]`        | is one of a set                           |
| `attr > 5` (`< >= <=`)      | numeric comparison                        |
| `attr == $v` / `attr != $v` | equals / differs from a captured value    |

### Scoping a rule

A rule applies everywhere by default. Narrow it by language and by path glob:

```
rule NO_PROVIDER_SDK_OUTSIDE_INFRA (error) {
  lang typescript, javascript, go, python
  in  "services/**", "apps/**"
  not in "**/infrastructure/**"
  match Import[source ~ /^(aws-sdk|@aws-sdk\/.*|stripe|twilio|googleapis)$/]
  message "Use provider SDKs only from the approved infrastructure layer."
}
```

`lang` lists the languages the rule covers; `in` / `not in` include and exclude
files by glob.

### Calls, arguments, and receivers

Calls can be written the way they appear in source, with positional arguments —
no need to spell out attribute names:

```
log(_, $level: string, ..)          # bind the 2nd arg; it must be a string literal
db.query($sql: string)              # a one-arg method call on any `db`
$client.send($msg, ..)              # capture the receiver and the first argument
```

- A **receiver** is the object before the method: `obj.method(…)` matches a
  literal, `$obj.method(…)` captures (or constrains) it.
- Each argument position is `_` (anything), a literal, or a `$capture`, with an
  optional `: kind`. The kind is the argument's **syntactic** form — `string`,
  `number`, `bool`, `identifier`, `member`, `call`, `object`, `array`,
  `function`, `regex`, `template` — i.e. *how it was written*, not an inferred
  static type.
- A trailing `..` means "and any further arguments"; without it the arity is
  exact.

### Captures and unification

A `$name` used for the first time **binds** to whatever it matched; every later
use of the same name is an **equality constraint**. That's how one rule
correlates two places by the *same* value — the same object, the same key, the
same identifier — with no extra ceremony.

### Sequences: order, pairing, and absence

A sequence matches an ordered run of constructs, optionally confined to a single
enclosing scope. Combined with capture, this expresses the classic
"acquired-but-never-released" family of rules — written as the shape that is
*itself* the violation: an acquire with no matching release in the same scope.

```
rule LISTENER_LEAK (warning) {
  sequence in scope {
    $obj.addEventListener($ev, ..)
    not $obj.removeEventListener($ev, ..) *
  }
  message "addEventListener has no matching removeEventListener (same object + event)."
}
```

Inside a sequence each step may carry:

- a quantifier — `?` (optional), `*` (zero or more), `+` (one or more);
- `not` — the step must **not** appear;
- alternation — `( A | B )` matches either;
- `any` — the wildcard kind, matching a construct of any kind;
- `as v = attr, …` — explicit captures, in addition to the `$…` sugar.

`in scope` anchors the whole sequence within one lexical block, so "begin
without commit" or "lock without unlock" is scoped to the block that opened it,
not the whole file. The matcher tries every starting position and reports every
occurrence.

### Asking about the surrounding scope

When you don't need a full sequence — just a question about the block a
construct sits in — use a scope predicate:

```
rule LOCK_WITHOUT_UNLOCK (error) {
  match Call[name ~ /acquire$/]
  where scope not contains Call[name ~ /release$/]
  message "A lock acquired here is never released in the same scope."
}
```

The clauses are `where scope contains …`, `where scope not contains …`, and
`where scope followed by …` (appears later in the same block).

### Composing and counting

Rules can be built from other rules as a final pass — set algebra over their
results, or a cardinality threshold:

```
rule HOTSPOT (warning) {
  compose intersection of TOUCHES_AUTH, TOUCHES_BILLING by file, function
  message "A function that reaches both auth and billing — split it."
}

rule TOO_MANY_ENV_READS (warning) {
  count RAW_ENV_ACCESS per file > 10
  message "This file reads the environment directly more than ten times."
}
```

`compose` takes `intersection`, `union`, or `difference`, keyed by any
attributes (`by file, function`). `count` compares per `file` or per `function`
with `>`, `<`, `>=`, `<=`, or `==`.

### Matching anything: the token layer

Constructs the structural layer doesn't name are still reachable — match the
token by its native `node_kind`:

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

Token predicates include `node_kind`, `text`, `function` (the enclosing
function), `named`, the nesting depths `curly_depth` / `round_depth` /
`bracket_depth`, `range_lines`, and `text_len`. `codepolicy events <file>
--tokens` prints the available `node_kind`s for a file. Token rules match a
single construct; correlation and ordering belong to the structural layer above.

### Exceptions, inline

A rule can carry its own escape valves, so an approved exception doesn't mean
disabling the rule:

```
rule NO_UNAPPROVED_STATE_LIBRARY (error) {
  match PackageAdded[name in ["redux", "recoil", "mobx", "xstate"]]
  unless adr "state management exception"
  message "Pick a state library through an architecture decision record."
}
```

`unless path "glob"` exempts matching files, `unless adr "topic"` defers to an
accepted decision record, and `unless waiver` honors a file-scoped waiver.

### Severity, messages, comments

The `(error)` / `(warning)` after the rule name is its severity — errors fail a
run, warnings don't. `message "…"` is the remediation shown on a hit; `desc
"…"` documents the rule; `#` starts a comment.

### YAML, if you prefer

Every rule has an equivalent YAML form, and `check` reads either. The grammar
above is the same engine; YAML is handy when rules are generated or kept
alongside other structured config. `codepolicy init` writes a starter pack in
either format (`--format rules` or `--format yaml`).

## Escape hatches

Two repo-level mechanisms complement the inline `unless` guards:

- **Waivers** — small YAML files under `.codepolicy/waivers/`, each naming a
  `rule` and a `file`. A waiver is a global, file-scoped suppression: it silences
  that rule in that file regardless of whether the rule opted in.
- **ADRs** — architecture decision records under `docs/adr/`, each with a
  `topic` and a `status`. An accepted ADR satisfies any `unless adr "topic"`
  guard repo-wide, so a documented, deliberate decision lifts the rule.

Both directories are configurable.

## Running it

```bash
codepolicy init                          # write a starter rule pack
codepolicy check                         # check the current repo
codepolicy check services/ --format agent  # LLM-friendly output for a subtree
codepolicy check --rules my.rules --format json
codepolicy events path/to/File          # dump the structural constructs for a file
codepolicy events path/to/File --tokens # ...and the token layer
codepolicy explain-rule NO_DEBUGGER     # show how a rule is interpreted
```

`check` discovers a `codepolicy.rules` or `codepolicy.yaml` at the root (or take
`--rules <file>`), scans every supported file in parallel, and prints violations
in `human`, `json`, or `agent` format. It exits `1` when any **error**-severity
violation is found, `0` when clean, and `2` on a usage or I/O error — drop it
straight into CI or an agent loop.

## Languages

The engine — the grammar, the matcher, and both code layers — is
language-agnostic. Language support is a plug-in: a **frontend** turns a file
into the token layer, the structural layer, or both. Frontends ship for
TypeScript / JavaScript (via tree-sitter) and for package manifests; adding
another language is a single trait implementation, and a rule written against
the structural layer applies to it the moment its frontend exists.

### Adding a language frontend

The whole contract is one trait:

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

A frontend can produce just the token layer (a plain lexer is enough — no parse
tree required), just the structural layer (like the manifest frontend), or both
from one parse (like the tree-sitter frontend). The token layer is built only
when `want_tokens` is set, which the pipeline does only if a loaded rule
references `Token` — so rules that never touch the token layer pay nothing for
it.

To add one: implement the trait in a new module under
`codepolicy-frontends/src/`, register it in `default_frontends()`, and add a
fixture plus an assertion in `crates/codepolicy-cli/tests/fixtures.rs`. Honor
the shared conventions — 1-based, end-exclusive spans; set the `language` on
everything you emit; reuse the canonical attribute vocabulary so existing rules
apply unchanged (`Import` carries `source` + `symbols`; `Call` carries `name`,
`callee`, `receiver`, `string_args`, `arg_count`, and per-position
`argN`/`argN_kind`; and so on). The two shipped frontends are the templates:
`ts_js.rs` (parser, both layers) and `manifest.rs` (structured-only).

## Architecture

A Cargo workspace:

| Crate                  | Responsibility                                                       |
| ---------------------- | ------------------------------------------------------------------- |
| `codepolicy-events`    | The structural model (`Event` / `EventKind` / `Span` / `Language`) and the compact token layer (`Token` / `TokenStream` / `Interner`) |
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

The design rationale lives in [`../proposal.md`](../proposal.md).
