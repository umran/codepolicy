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

## Why now

Pattern-based static analysis over a token stream — Cobra's model — never
displaced general-purpose linters and code review on most teams. Bespoke
structural rules cost more to write and maintain than they return when humans
produce code at human speed: the invariants worth enforcing are few, and review
catches the rest.

LLM-generated code changes that arithmetic:

- **Volume outruns review.** A model emits more code than anyone reads closely. A
  deterministic check costs almost nothing per file and scales with output;
  human attention does not.
- **Generation is probabilistic; a rule is not.** A model can break a
  project-specific invariant fluently and plausibly. A pattern either matches or
  it doesn't, independent of model or prompt — turning "the model usually does X"
  into "X holds, or the run fails."
- **The authoring cost collapses.** Writing a rule from a described invariant is
  exactly what a model is good at, and the rule is then a small, inspectable,
  version-controlled artifact — not a prompt you hope was honored.

The leverage is largest for the invariants no off-the-shelf tool encodes, the
ones specific to your codebase: "this call only inside that module," "every
subscription is torn down in the same block." The `agent` output format closes
the loop — each violation is machine-readable (`rule`, `file:line`, fix message),
so an agent can correct its own output before a human sees it.

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
rule no_explicit_any (warning) { # severity is (error) or (warning); default is error
  lang typescript, javascript    # optional; omit to apply to all languages
  in  "src/**"                   # optional include globs (a file must match one)
  not in "**/*.test.ts"          # optional exclude globs (excludes win)
  match "any"
  message "Avoid the `any` type; use a precise type or `unknown`."
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

## Matching a lexeme

A `match` body names one token pattern, written in Cobra's token syntax:

```
match var                          # a literal lexeme (the `var` keyword)
match "=="                         # quote operators and punctuation (loose equality)
match @ident                       # a token class: any identifier
match @ident & /^_/                # an identifier whose name starts with `_`
match /^use[A-Z]/                  # a React-hook identifier: useState, useMemo, …
match @comment & /@ts-ignore/      # a // @ts-ignore suppression comment
match @comment & /TODO/ & ^/\(#\d+\)/   # a TODO with no (#123) issue reference
```

- A **bare word** (`var`, `console`, `addEventListener`) matches a lexeme by its
  exact text — you name a keyword, identifier, or property the way it appears in
  the source. A bare word works only when it isn't already rule syntax: `==` is a
  comparison, `{` opens a block, `&` conjoins, `any` is the wildcard — so quote
  those (and other operators and delimiters) to match the literal lexeme: `"=="`,
  `"=>"`, `"{"`, `"any"`.
- **`@class`** matches the normalized, language-neutral class the frontend
  assigns — `@ident`, `@prop`, `@str`, `@num`, `@bool`, `@regex`, `@comment`,
  `@symbol` — so `@ident` means "identifier" in any language with a frontend.
- **`/regex/`** matches a lexeme whose text matches the regex; **`^/regex/`**
  requires that it does not. A regex tests every lexeme's text, strings and
  comments included — conjoin a `@class` to narrow it.
- **`&`** conjoins atoms on a *single* lexeme: `@ident & /^_/` is one lexeme that
  is both an identifier and starts with `_`.
- **`any`** (or Cobra's **`.`**) matches one lexeme of any kind (the sequence
  wildcard, below).

There is no `statement_block` or `call_expression` lexeme — those are composite
constructs, not lexemes. Match a block by its `{`, a call by the callee lexeme
followed by `(`, and so on. `*` is never a character wildcard: inside `/…/` it is
the regex Kleene star, and a bare `*` is token-level repetition on a sequence
step.

## Sequences

A sequence matches an ordered run of lexemes. The matcher tries every start
position; from there the steps must consume the region to its end. `in scope`
makes the region each `{}` block (paired via the matching-delimiter links);
without it the region is the whole file.

```
sequence in scope {
  fetch                         # a fetch(...) call
  any *                         # any run of lexemes  (quantifiers: ? 0–1, * 0+, + 1+)
  ( then | catch )              # followed by .then or .catch  (or [ then catch ])
  any *                         # ... and the rest of the block (matching is end-anchored)
}
```

This matches a `fetch(...)` followed by a `.then`/`.catch` in the same block. The
trailing `any *` is needed because a sequence must consume its region to the end.

- `any` (or `.`) matches one lexeme of any kind; `any *` (or `.*`) soaks the rest
  of a region.
- Alternation is `( a | b )` or Cobra's space-separated set `[ a b ]`.
- `not X` (or Cobra's `^X`) consumes exactly one lexeme that must not match `X`;
  `not X *` consumes a run of non-`X` lexemes.
- **Delimiters balance.** When `( )`, `{ }`, or `[ ]` appear as explicit steps,
  each pairs with its matching partner (via the lexer's links), so a wildcard
  between them cannot run past the close: `for "(" any * ")"` matches a `for`'s own
  parentheses, not some later `)`.
- Because matching is end-anchored, a trailing `not X *` means "no `X` in the rest
  of the region" — the use-without-cleanup idiom:

```
rule listener_without_cleanup (warning) {
  sequence in scope {
    addEventListener
    not removeEventListener *
  }
  message "addEventListener with no removeEventListener in the same block."
}
```

**Captures** correlate two lexemes by text, exactly as in Cobra. `x:@ident` binds
the matched lexeme's text to `x`; a later `:x` is a backreference — the same text
again. This flags a self-assignment, `x = x;`:

```
rule no_self_assign (warning) {
  sequence in scope {
    x:@ident       # bind the identifier's text to x
    "="
    :x             # the same identifier on the right
    ";"
    any *          # absorb the rest of the block (matching is end-anchored)
  }
  message "Self-assignment has no effect."
}
```

A construct with arguments emits its child lexemes after it, so in a *pairing*
sequence put `any *` between and after the steps to absorb them.

## Scope predicates

A `match` rule can ask about the lexeme's enclosing `{}` block without a full
sequence:

```
rule console_in_returning_block (warning) {
  match console
  where scope contains return
  message "A console.* left in a block that returns a value."
}
```

Clauses: `where scope contains …`, `where scope not contains …`, and `where scope
followed by …` (order-sensitive — appears later in the block). Multiple clauses
are ANDed.

## Composition and counting

A rule can be derived from other rules in a pass that runs after the matching
rules have produced their violations:

```
rule uses_console (warning) { match console }
rule loose_equality (warning) { match "==" }

rule sloppy_function (error) {
  compose intersection of uses_console, loose_equality by file, function
  message "A function that uses console.* and loose equality."
}

rule too_many_console (error) {
  count uses_console per function > 1
  message "More than one console.* call in a function."
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
rule no_console (warning) {
  match console
  unless path "**/scripts/**"   # by glob
  unless adr "logging-in-cli"   # an accepted decision record (repo-wide)
  message "Use the logger instead of console.*"
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
codepolicy explain-rule no_explicit_any    # show how a rule compiles
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
