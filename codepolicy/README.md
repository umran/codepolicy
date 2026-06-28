# codepolicy

codepolicy checks a codebase against rules you define and reports each violation
with its file and line. A rule is a structural constraint over code — an import
allowed only in certain paths, a call that must be paired with another in the
same scope, a banned construct, a dependency that needs sign-off. The tool has no
built-in notion of correct code; it enforces the rules you write.

Exit codes: `0` clean, `1` an error-level violation was found, `2` the rules file
is malformed. So it drops into a pre-commit hook, a CI gate, or an agent loop.

The approach follows NASA/JPL's Cobra: pattern rules over a flat stream of code
facts, each match carrying file and line evidence.

## The two layers

A *frontend* turns a source file into up to two layers. **Which layers a frontend
produces is its own implementation choice.**

- The **token layer** is a flat stream of every lexical/syntactic token. Producing
  it needs only a lexer. It is, in effect, a linked list of tokens (each token
  knows its neighbours and its matching delimiter), and token rules are regular —
  one token, tested by equality, regex, or a numeric field. Scanning it is linear
  and cheap.
- The **canonical event layer** is a set of *normalized* constructs — `Import`,
  `Call`, `TypeDecl`, `EnvAccess`, `ScopeStart`, … — with structured attributes.
  Producing it requires understanding structure, i.e. a parse.

A frontend may emit only tokens (a lexer), only canonical events (the bundled
`package.json` reader does this), or both from one parse (the bundled
TypeScript/JavaScript frontend does this).

**What the canonical layer buys you over raw tokens.** Tokens give you regular,
single-token matching. Canonical events add three things the token layer cannot
express:

1. *Normalized, cross-language kinds.* `Import` means the same thing in any
   language with a frontend, so one rule ports across languages.
2. *Structured attributes.* A `Call` carries `name`, `receiver`, `arg0…argN`,
   `arg_count`; an `Import` carries `source` and `symbols` — instead of raw token
   text you would otherwise have to re-parse.
3. *Relational matching.* Sequences, scope predicates, captures/backreferences,
   and composition all operate on events. Token rules are single-construct only.

**The canonical vocabulary is fixed and deliberately small** (`File`,
`PackageAdded`, `Import`, `Export`, `Call`, `NewExpr`, `TypeDecl`, `FunctionDecl`,
`MethodDecl`, `ClassDecl`, `Attribute`, `EnvAccess`, `StringLiteral`, `Comment`,
`ScopeStart`, `ScopeEnd`, `GraphqlOperation`, `GeneratedFile`, `ConfigFile`,
`Token`), and a frontend only fills the kinds and attributes it implements. For
any construct outside that set — a `switch`, a ternary, a decorator, a specific
operator — you reach for the token layer, which can match any node. Expect most
arbitrary, project-specific rules to use the token layer; the canonical layer is
where cross-construct logic lives. Extending the canonical layer (new kinds, more
attributes) is a frontend change.

**Cost.** The token stream is cheap: a flat array matched by regular expressions.
The parse that produces canonical events is the heavier operation, and at millions
of lines it is the dominant cost — so a token-only frontend is far cheaper than a
parsing one. (In the current TS/JS frontend both layers come from a single
tree-sitter parse, so that parse is paid whenever events are needed; the token
array itself is built only when a loaded rule references the `Token` kind.)

Before writing a rule, dump what a file actually produces:

```bash
codepolicy events path/to/File          # canonical events with their attributes
codepolicy events path/to/File --tokens # the token layer (node kinds, fields)
```

## Writing rules

Rules are written in a textual grammar (`codepolicy.rules`). Examples below are
the actual syntax; each was run against the tool.

### Rule shape, severity, comments

```
# a comment runs to end of line, anywhere
rule no_console (warning) {        # severity is (error) or (warning)
  match Call[name = "console"]
  message "Use the logger."
}
```

`(error)` fails the run; `(warning)` is reported but exits `0`. Omitting the
parens defaults to **error**. The smallest legal rule is `rule NAME { <body> }`.
Rule names are identifiers — letters, digits, underscore; **no hyphens**. A file
holds many rules, separated by blank lines.

`desc "…"` documents a rule and `message "…"` is the remediation; both are
optional. In `agent` output they render as the `why:` and `fix:` lines.

### Language and path scope

```
rule no_provider_sdk (error) {
  lang typescript, javascript        # omit `lang` to apply to all languages
  in  "services/**", "apps/**"       # include globs (a file must match one)
  not in "**/generated/**"           # exclude globs
  match Import[source ~ /^aws-sdk$/]
  message "Provider SDKs belong in the infrastructure layer."
}
```

A file is in scope iff it matches an include **and** matches no exclude —
**excludes win**. Globs match the path relative to the directory being checked
(e.g. `services/api/x.ts`), and `*` crosses `/`, so anchor with `**/dir/**` or
`dir/**` rather than a bare token. Scope is per-rule.

### Matching one construct

`match Kind[pred, pred]` matches a construct whose attributes satisfy every
predicate (predicates are ANDed). `match Kind` with no brackets matches every such
construct.

| Predicate                   | Holds when                                                |
| --------------------------- | --------------------------------------------------------- |
| `attr = "v"` / `attr = 5`   | a value equals `v` (equality coerces string ↔ number)     |
| `attr ~ /re/`               | a value matches the regex (unanchored)                    |
| `attr !~ /re/`              | no value matches the regex                                |
| `attr in ["a", "b"]`        | a value is in the set                                     |
| `attr > 5` (`< >= <=`)      | a numeric value compares true                             |
| `attr == $v` / `attr != $v` | a value equals / differs from a captured variable         |

```
match Import[source = "express"]                 # exact string
match Call[arg_count > 2]                         # numeric, on a structural attr
match Comment[text ~ /TODO/]                      # regex search
match EnvAccess[name !~ /^AWS_/]                  # negated regex
match Call[name in ["eval", "exec", "fetch"]]     # membership
match TypeDecl[decl_kind = "interface"]           # interface vs type
match EnvAccess                                   # bare kind: every match
```

Two semantics to internalize:

- **List attributes are tested element-wise.** `Import.symbols` is a list, so
  `symbols = "useQuery"`, `symbols in ["useQuery"]`, and `symbols ~ /^use/` each
  hold if *any* element does. Same for `string_args`.
- **A predicate on an attribute the kind doesn't carry never matches** — it is not
  an error. A typo'd attribute silently disables the rule, so confirm names with
  `codepolicy events` first.

Every event also carries structural attributes you can match on: `curly_depth`,
`round_depth`, `bracket_depth` (nesting at the construct), `range_lines`,
`text_len`, and `function` (the enclosing function, when inside one).

### Calls

Calls can be written with positional arguments after `match`. This desugars to
predicates on `name`, `receiver`, `arg0…argN`, `argN_kind`, and `arg_count`.

```
match foo(_, _, _)             # exactly three arguments (arg_count = 3)
match foo(_, _, ..)            # two or more (arg_count >= 2); `..` must be last
match db.query(_)              # literal receiver: name="query", receiver="db"
match $obj.query(_)            # any receiver, but there must be one
match exec("rm", force)        # arg0 is the string "rm"; arg1 is the identifier `force`
match run(42)                  # arg0 is the numeric literal 42
match handle($a: string)       # arg0's syntactic kind is string
```

Argument patterns: `_` (any), a quoted string literal, a bareword (an exact
identifier name), a number, or `$var`, each with an optional `: kind`. A kind is
the argument's *syntactic* form — `string`, `template`, `number`, `bool`,
`identifier`, `member`, `call`, `object`, `array`, `function`, `regex`, `null`,
`undefined`, `other` — taken from the parse node, not an inferred type. For a
member call, `name` is the method only and `callee` is the full `obj.method`.

Gotcha: inside a *single* call, a `$var` is just a wildcard — `copy($x, $x)`
matches any two arguments, not two equal ones. To require two arguments of one
call be equal, use `match Call[arg0 == $x, ...]`-style backreferences, or correlate
across sequence steps (below). Captures across steps do work.

### Captures and unification

`$x` binds the first time it appears and constrains every later appearance to the
same value. Binding is per-rule. Across sequence steps this correlates two
constructs by a shared value:

```
sequence in scope {
  Call[name = "lock"] as r = arg0      # bind r from arg0
  any *
  Call[name = "unlock", arg0 == $r]    # require the same arg0
  any *
}
```

`as v = attr` is the explicit form; the `$r` in `lock($r: string)` call-sugar is
the shorthand. Inside `Kind[…]`, `==` and `!=` are *always* backreferences (use
`attr = "v"` for a literal).

### Sequences

A sequence matches an ordered run of constructs. The matcher tries every start
position; from there the steps must consume the region to its end. `in scope`
makes the region each `{}` block's interior; without it the region is the whole
file.

```
sequence in scope {
  validate()
  log() *           # quantifiers: ? (0–1), * (0+), + (1+) on the preceding step
  save()
}
```

```
sequence in scope {
  start()
  ( stop() | pause() )    # alternation; each arm is one matcher
}
```

```
sequence in scope {
  init()
  any              # one construct of any kind; `any *` soaks the rest
  done()
}
```

- A bare `not X` consumes exactly one construct that must not match `X`. A
  quantified `not X *` consumes a run of non-`X` constructs.
- Because matching is end-anchored, a trailing `not X *` means *"and no `X` in the
  rest of the region."* This is the acquire-without-release idiom:

```
rule listener_leak (warning) {
  sequence in scope {
    $obj.addEventListener($ev, ..)
    not $obj.removeEventListener($ev, ..) *
  }
  message "addEventListener with no matching removeEventListener (same object + event)."
}
```

It fires when no matching removal exists, stays silent when one appears later in
the scope (intervening statements don't matter, because the negated `*` absorbs
them), and still fires if a removal targets a *different* captured object or
event. The negated `*` is what spans the gap — there is no separate wildcard
between the two steps.

Gotcha: a call with arguments emits a trailing `StringLiteral`/argument events
right after its `Call`. In a *pairing* sequence (`open(...)` … `close(...)`),
insert `any *` between and after the steps to absorb those, or the end-anchoring
fails. The leak idiom above avoids this because its trailing negated `*` already
absorbs everything.

### Scope predicates

A `match` rule can ask about the construct's enclosing `{}` scope without a full
sequence:

```
rule query_needs_getdb (error) {
  match $db.query(_)
  where scope not contains getDb()
  message "A query must run in a scope that also calls getDb()."
}
```

Clauses: `where scope contains …`, `where scope not contains …`, and `where scope
followed by …` (which is order-sensitive — the construct must appear *after* the
matched one in the same scope). Multiple clauses are ANDed. `where scope` attaches
only to a `match` body.

### Composition and counting

These run after the single and sequence rules, over their results.

```
rule uses_fetch (warning) { match fetch(_) }
rule uses_eval  (warning) { match eval(_) }

rule fetch_and_eval (error) {
  compose intersection of uses_fetch, uses_eval by function
  message "One function uses both fetch and eval."
}

rule too_many_fetch (error) {
  count uses_fetch per file > 2
  message "More than two fetch calls in this file."
}
```

`compose` groups each referenced rule's violations by a key tuple (`by file`,
`by function`, or both; default `file`). `intersection` keeps keys present in
every referenced rule, `union` keys in any, `difference` keys in the first rule
and none of the rest. It emits one violation per surviving key. `count` groups one
rule's violations per `file` or `function` and fires when the group size compares
true (`>`, `<`, `>=`, `<=`, `==`) to the threshold. Referenced base rules still
report on their own — make them `warning` to keep a composed `error` clean.

### The token layer

`match Token[…]` matches one node of the parse by its native tree-sitter
`node_kind`. Use `codepolicy events <file> --tokens` to find the exact names.

```
match Token[node_kind = "switch_statement"]              # a construct with no canonical kind
match Token[node_kind = "ternary_expression"]
match Token[node_kind = "=="]                             # the operator symbol itself
match Token[node_kind ~ /^={2,3}$/]                       # node_kind takes a regex too
match Token[node_kind = "identifier", text ~ /_/]         # snake_case identifiers
match Token[node_kind = "statement_block", curly_depth > 2]   # over-nested blocks
match Token[node_kind = "function_declaration", range_lines > 3]  # long functions
match Token[node_kind = "string", text_len > 20]          # long string literals
```

Token fields: `node_kind`, `text`, `function`, `named`, `curly_depth` /
`round_depth` / `bracket_depth`, `range_lines`, `text_len`. `named` distinguishes a
parser-named construct from a same-spelled bare symbol and **must be written as a
string**: `named = "true"` selects the numeric literal `1`; `named = "false"`
selects the bare `number` keyword token. Token rules are single-construct: a
`sequence` or `where scope` over `Token` is rejected at load time. For ordered or
correlated logic, use the canonical kinds.

### Exceptions

A rule can exempt cases inline rather than being switched off:

```
rule no_env_access (error) {
  match EnvAccess
  unless adr "direct-env-access"     # repo-wide, if an accepted ADR exists
  unless path "**/scripts/**"        # by glob
  message "Use the config module."
  unless waiver                      # file-scoped waiver under this rule's id
}
```

- `unless path "glob"` exempts files by glob.
- `unless adr "topic"` defers to an accepted decision record (repo-wide).
- `unless waiver` honors a file-scoped waiver under this rule's id; `unless waiver
  other_id` uses a different id. Place a **bare** `unless waiver` last in the body
  — the parser otherwise consumes the next keyword (`message`, …) as the waiver
  id. The explicit `unless waiver other_id` form has no such restriction.

## Escape-hatch files

- **Waivers** — YAML under `.codepolicy/waivers/`, each with a `rule` and a `file`
  key. A waiver suppresses that rule in that file *unconditionally* — even without
  an `unless waiver` in the rule. The `file` is an **exact** root-relative path
  (e.g. `apps/legacy.ts`), not a glob.
- **ADRs** — YAML under `docs/adr/`, each with `topic` and `status`. A record with
  `status: accepted` (case-insensitive) satisfies any `unless adr "topic"` guard
  for that topic, repo-wide.

Relocate either directory with top-of-file directives, which replace the defaults:

```
waivers "policy/waivers"
adrs "policy/decisions"
```

## YAML form

Every rule has an equivalent YAML form, and `check` reads either; both compile to
the same representation. `codepolicy init` writes a starter pack as `--format
rules` or `--format yaml`.

## Running it

```bash
codepolicy init                            # write a starter rule pack
codepolicy check                           # check the current repo
codepolicy check services/ --format agent  # check a subtree, agent output
codepolicy check --rules my.rules --format json
codepolicy events path/to/File [--tokens]  # inspect what a file produces
codepolicy explain-rule no_env_access      # show how a rule compiles
```

`check` discovers a `codepolicy.rules` or `codepolicy.yaml` at the root, or takes
`--rules <file>`, and scans supported files in parallel. The three output formats:

- **human** — per violation: `SEVERITY rule_id`, `file:line:col`, the description,
  a `Matched:` line, a `Remediation:` line; a footer counts errors and warnings.
- **json** — `{ "violations": [...], "summary": { errors, warnings, total } }`.
- **agent** — terse: `SEVERITY rule_id at file:line:col`, then `matched:`, `why:`,
  `fix:` lines:

```
WARNING listener_leak at apps/widget.ts:3:3
  matched: Call name="addEventListener" receiver="el" arg0="click" ...
  fix: addEventListener with no matching removeEventListener (same object + event).

codepolicy: 0 error(s), 1 warning(s). Fix the errors above before committing.
```

## Languages and frontends

The grammar, the matcher, and both layers are language-independent. A language is
supported by a frontend, and the frontend decides which layers it provides. The
contract is one trait:

```rust
pub struct SourceFile<'a> { pub path: Utf8PathBuf, pub text: &'a str }

#[derive(Default)]
pub struct Extracted {
    pub tokens: Option<TokenStream>, // the token layer (a lexer is enough)
    pub events: Vec<Event>,          // the canonical layer (requires a parse)
}

pub trait LanguageFrontend: Sync {
    fn name(&self) -> &'static str;
    fn supports_file(&self, path: &Utf8Path) -> bool;
    fn extract(&self, file: &SourceFile<'_>, want_tokens: bool) -> anyhow::Result<Extracted>;
}
```

`want_tokens` is set only when a loaded rule references the `Token` kind, so a
frontend can skip building the token array otherwise. To add a language, implement
the trait under `codepolicy-frontends/src/`, register it in `default_frontends()`,
and add a fixture and assertion in `crates/codepolicy-cli/tests/fixtures.rs`. Use
1-based, end-exclusive spans, set `language` on everything emitted, and reuse the
attribute names above so existing rules apply unchanged. The shipped frontends are
`ts_js.rs` (parser, both layers) and `manifest.rs` (canonical events only).

## Architecture

A Cargo workspace:

| Crate                  | Responsibility                                                       |
| ---------------------- | ------------------------------------------------------------------- |
| `codepolicy-events`    | The canonical model (`Event` / `EventKind` / `Span` / `Language`) and the token layer (`Token` / `TokenStream` / `Interner`) |
| `codepolicy-frontends` | `LanguageFrontend` and the shipped frontends                        |
| `codepolicy-rules`     | Rule grammar (textual DSL + YAML), compilation, the predicate language |
| `codepolicy-match`     | Indexing and matching: single construct, sequences, scope predicates, compose/count, token rules, waiver/ADR/`unless` |
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
