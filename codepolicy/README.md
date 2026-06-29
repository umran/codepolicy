# codepolicy

codepolicy is a rule engine for source code. You write rules — declarative
patterns over a file's **lexemes** — and it scans a repository and reports every
match as `file:line` evidence with a message. It has no built-in notion of
correct code; it enforces exactly the rules you write.

Exit codes: `0` clean, `1` an error-level violation was found, `2` the rules file
is malformed — so it fits a pre-commit hook, a CI gate, or an agent loop.

The model follows NASA/JPL's Cobra: a flat stream of lexical tokens, matched by
patterns, never raw text — so a rule for the identifier `x` matches the lexeme
`x`, not the `x` inside `"prefix"` or a comment.

## How it sees code

A frontend lexes each file into a flat, ordered **token stream**: one entry per
lexeme — keywords, operators, identifiers, number/string literals, punctuation,
comments. A string or template literal is a *single* token (matching never
reaches inside it). Producing the stream needs only a lexer; no parse tree is
built, and scanning it is linear.

Each token carries:

- `node_kind` — the lexeme's grammar name (`identifier`, `switch`, `==`, `{`, `string`, `comment`)
- `class` — a normalized, language-neutral class: `ident`, `prop`, `str`, `num`, `bool`, `regex`, `comment`, `symbol`
- `text` — the source text
- `named` — a named lexeme (`identifier`, `string`, …) vs. a bare symbol/keyword/operator
- `function` — the enclosing function, if any
- `curly_depth`, `round_depth`, `bracket_depth` — nesting at the lexeme
- `range_lines`, `text_len` — lines / bytes the lexeme spans

Each token also links to its matching delimiter (`{`↔`}`, `(`↔`)`, `[`↔`]`),
which is how scopes are defined. Inspect a file's stream with `codepolicy tokens
<file>`.

## Rule basics

Rules are written in a textual grammar (`codepolicy.rules`); each also has an
equivalent YAML form, and `check` reads either. A rule is an envelope around one
matching body:

```
# a comment runs to end of line, anywhere
rule no_debugger (error) {     # severity is (error) or (warning); default is error
  lang typescript, javascript  # optional; omit to apply to all languages
  in  "src/**"                 # optional include globs (a file must match one)
  not in "**/*.test.ts"        # optional exclude globs (excludes win)
  match Token[node_kind = "debugger"]
  message "Remove debugger statements."
}
```

- **Severity.** `(error)` fails the run; `(warning)` is reported but exits `0`.
  Omitting the parens defaults to error. Rule names are identifiers — letters,
  digits, underscore; no hyphens. A file holds many rules, separated by blank
  lines.
- **Description and message.** `desc "…"` documents a rule; `message "…"` is the
  remediation. Both optional; in `agent` output they render as `why:` and `fix:`.
- **Scope.** `lang` restricts by language. `in` / `not in` filter by glob; a file
  is in scope iff it matches an include **and** no exclude (excludes win). Globs
  match the path relative to the checked directory; `*` crosses `/`, so anchor
  with `**/dir/**` or `dir/**`. Scope is per-rule.

The predicate operators inside `Token[ … ]` (all ANDed):

| Predicate                   | Holds when                                          |
| --------------------------- | --------------------------------------------------- |
| `field = "v"` / `field = 5` | a value equals `v` (equality coerces string ↔ number) |
| `field ~ /re/`              | a value matches the regex                           |
| `field !~ /re/`             | no value matches the regex                          |
| `field in ["a", "b"]`       | a value is in the set                               |
| `field > 5` (`< >= <=`)     | a numeric value compares true                       |
| `field == $v` / `field != $v` | equals / differs from a captured variable (sequences) |

A predicate on a field a token doesn't carry never matches — confirm names with
`codepolicy tokens` first.

## Matching a lexeme

Cobra's token syntax is supported directly. Match a lexeme by **class**, by
**exact** text, by a **regex** over its text, or by explicit fields; conjoin
constraints on one token with `&`:

```
match @ident                       # any identifier
match @ident & /^pumba/            # an identifier whose name starts with pumba
match /^use/                       # any lexeme whose text matches the regex
match Token[text = "switch"]       # the exact `switch` lexeme
match Token[node_kind = "=="]      # an operator lexeme
match Token[node_kind = "identifier", text ~ /_/]      # snake_case identifiers
match Token[node_kind = "string", text_len > 80]       # a long string literal
match Token[class = "comment", text ~ /FIXME/]         # a comment mentioning FIXME
match Token[node_kind = "{", curly_depth > 2]          # a brace nested past 2 blocks
```

The fields are lexeme-level: there is no `statement_block` or `call_expression`
token (those are composite constructs, not lexemes) — match a block by its `{`,
a call by the callee lexeme followed by `(`, and so on.

`@class` is a normalized, language-neutral class the frontend assigns, so
`@ident` means "identifier" in any language with a frontend. `*` is never a
character wildcard — text patterns are regexes (where `*` is the Kleene star
*inside* the slashes); a bare `*` is token-level repetition on a sequence step.

This mirrors Cobra one-to-one:

| Cobra | here |
| --- | --- |
| `for` (exact lexeme) | `Token[text = "for"]` |
| `/^pumba/` (text regex) | `/^pumba/` |
| `@ident` (token class) | `@ident` |
| `@ident & /^pumba/` | `@ident & /^pumba/` |
| `.` (any token) | `any` |
| `*` `+` (token repetition) | `*` `+` `?` on a step |
| `x:@ident … @x` | `… as x = text … text == $x` |

A `/regex/` matches a token's text, string and comment tokens included — add a
`@class` to exclude them (e.g. `@ident & /^pumba/`).

## Sequences

A sequence matches an ordered run of lexemes. The matcher tries every start
position; from there the steps must consume the region to its end. `in scope`
makes the region each `{}` block (paired via the matching-delimiter links);
without it the region is the whole file.

```
sequence in scope {
  Token[text = "validate"]
  any *                         # quantifiers: ? (0–1), * (0+), + (1+)
  ( Token[text = "save"] | Token[text = "commit"] )   # alternation
}
```

- `any` matches one lexeme of any kind; `any *` soaks the rest of a region.
- A bare `not X` consumes exactly one lexeme that must not match `X`; `not X *`
  consumes a run of non-`X` lexemes.
- Because matching is end-anchored, a trailing `not X *` means "no `X` in the rest
  of the region" — the acquire-without-release idiom:

```
rule lock_without_unlock (error) {
  sequence in scope {
    Token[text = "lock"]
    not Token[text = "unlock"] *
  }
  message "lock() with no unlock() in the same block."
}
```

**Captures** correlate two lexemes by a shared value. `as v = field` binds; a
later `field == $v` is a backreference:

```
sequence {
  Token[class = "ident"] as n = text     # bind the identifier text
  any *
  Token[class = "ident", text == $n]     # the same identifier again
  any *
}
```

A construct with arguments emits its child lexemes after it, so in a *pairing*
sequence put `any *` between and after the steps to absorb them.

## Scope predicates

A `match` rule can ask about the lexeme's enclosing `{}` block without a full
sequence:

```
rule debugger_in_returning_block (warning) {
  match Token[node_kind = "debugger"]
  where scope contains Token[node_kind = "return"]
  message "A debugger in a block that returns."
}
```

Clauses: `where scope contains …`, `where scope not contains …`, and `where scope
followed by …` (order-sensitive — appears later in the block). Multiple clauses
are ANDed.

## Composition and counting

A rule can be derived from other rules in a pass that runs after the matching
rules have produced their violations:

```
rule uses_fetch (warning) { match Token[text = "fetch"] }
rule has_debugger (warning) { match Token[node_kind = "debugger"] }

rule fetch_with_debugger (error) {
  compose intersection of uses_fetch, has_debugger by file, function
  message "A function that calls fetch still has a debugger."
}

rule too_many_debuggers (error) {
  count has_debugger per function > 1
  message "More than one debugger in a function."
}
```

`compose` groups each referenced rule's violations by a key tuple (`by file`,
`by function`, or both; default `file`). `intersection` keeps keys present in
every referenced rule, `union` keys in any, `difference` keys in the first rule
and none of the rest; it emits one violation per surviving key. `count` groups
one rule's violations per `file` or `function` and fires when the group size
compares true (`>`, `<`, `>=`, `<=`, `==`) to the threshold. Prefer `by file,
function` over bare `by function` (a function name is only unique within a file).

## Escape hatches

A rule can exempt cases inline:

```
rule no_debugger (error) {
  match Token[node_kind = "debugger"]
  unless path "**/scripts/**"   # by glob
  unless adr "debugging-tools"  # an accepted decision record (repo-wide)
  message "Remove debugger statements."
  unless waiver                 # a file-scoped waiver under this rule's id
}
```

Two repo-level files back these:

- **Waivers** — YAML under `.codepolicy/waivers/`, each with a `rule` and a `file`
  key. A waiver suppresses that rule in that file unconditionally (the `file` is
  an exact root-relative path).
- **ADRs** — YAML under `docs/adr/`, each with `topic` and `status`. A record
  with `status: accepted` (case-insensitive) satisfies any `unless adr "topic"`
  guard with that topic.

Relocate either directory with top-of-file directives: `waivers "dir"`, `adrs
"dir"`.

## Running it

```bash
codepolicy init                            # write a starter rule pack
codepolicy check                           # check the current repo
codepolicy check src/ --format agent       # check a subtree, agent output
codepolicy check --rules my.rules --format json
codepolicy tokens path/to/File             # dump a file's lexeme stream
codepolicy explain-rule no_debugger        # show how a rule compiles
```

`check` discovers a `codepolicy.rules` or `codepolicy.yaml` at the root, or takes
`--rules <file>`, and lexes supported files in parallel. The three output
formats: **human** (per-violation block with a `Matched:` line and remediation),
**json** (`{ "violations": [...], "summary": { errors, warnings, total } }`), and
**agent** (terse `SEVERITY rule_id at file:line:col` with `matched:` / `why:` /
`fix:` lines).

## Languages and frontends

A language is supported by a *frontend* that lexes a file into its token stream.
The contract is one trait:

```rust
pub struct SourceFile<'a> { pub path: Utf8PathBuf, pub text: &'a str }

pub trait Frontend: Sync {
    fn name(&self) -> &'static str;
    fn supports_file(&self, path: &Utf8Path) -> bool;
    fn lex(&self, file: &SourceFile<'_>) -> anyhow::Result<TokenStream>;
}
```

The bundled frontend is `ts_js.rs` (TypeScript/JavaScript via tree-sitter). To
add a language, implement the trait under `codepolicy-frontends/src/`, register
it in `frontends()`, and add a fixture and assertion in
`crates/codepolicy-cli/tests/fixtures.rs`. Use 1-based, end-exclusive spans, link
matching delimiters (`{`/`}`/`(`/`)`/`[`/`]`) via `jmp`, and assign each lexeme a
normalized `class` so `@class` rules apply unchanged.

## Architecture

A Cargo workspace:

| Crate                  | Responsibility                                                       |
| ---------------------- | ------------------------------------------------------------------- |
| `codepolicy-token`     | The lexeme model: `Token` / `TokenStream` / `Interner` / `Span` / `Language` |
| `codepolicy-frontends` | `Frontend` and the shipped TypeScript/JavaScript lexer              |
| `codepolicy-rules`     | Rule grammar (textual DSL + YAML), compilation, the predicate language |
| `codepolicy-match`     | Matching: single token, sequences, scope predicates, compose/count, waiver/ADR/`unless` |
| `codepolicy-report`    | `human` / `json` / `agent` rendering                                |
| `codepolicy-core`      | `Project`: discovery, parallel lexing, the check pipeline           |
| `codepolicy-cli`       | The `codepolicy` binary, the bundled starter pack, fixtures         |

## Build and test

```bash
cargo build --release         # binary at target/release/codepolicy
cargo test                    # unit + end-to-end fixture tests
cargo clippy --all-targets
```

Design rationale: [`../proposal.md`](../proposal.md).
