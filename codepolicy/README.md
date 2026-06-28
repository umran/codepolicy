# codepolicy

A fast, multi-language, **event-stream-based** static policy harness inspired by
NASA/JPL's Cobra analyzer. It enforces project-specific software-engineering
practices (architectural seams, approved access layers, config discipline) for
both humans and LLM coding agents, without bespoke AST analyzers per rule.

This is the **v0** implementation of [`../proposal.md`](../proposal.md). See the
proposal for the full design rationale.

## What it does

Source files are turned into a flat, language-neutral **canonical event stream**
(imports, calls, type declarations, env access, comments, …). Small declarative
YAML rules are compiled into indexed matchers over that stream, producing
violations with precise `file:line` evidence.

```
source files -> tree-sitter frontend -> canonical events -> indexed matcher -> violations
```

## v0 scope

- **Language:** TypeScript / JavaScript / TSX (via tree-sitter), plus a
  `package.json` manifest frontend for `PackageAdded`.
- **Events:** `File`, `PackageAdded`, `Import`, `Call`, `TypeDecl`,
  `FunctionDecl`, `EnvAccess`, `StringLiteral`, `Comment`, `ScopeStart`,
  `ScopeEnd`.
- **Rules:** the full declarative Cobra-parity rule language (**tiers 1–4**) —
  single-event rules (8 bundled, below); sequence rules (`match_sequence`) with
  order, quantifiers, alternation, negation, scope anchoring, and
  capture/backreferences (`bind`/`eq_ref`/`ne_ref`); structural-field comparisons
  (`.gt/.lt`); scope-relative predicates (`where_scope`); and rule composition /
  cardinality (`compose`/`count`). Proposal §8.6–8.10.
- **Output:** `human`, `json`, `agent`.
- **Escape hatches:** structured waivers and ADRs.

Bundled single-event rules: `NO_DIRECT_GRAPHQL_CLIENT`, `NO_RAW_GRAPHQL_FETCH`,
`NO_MANUAL_GRAPHQL_OPERATION_TYPES`, `NO_UNAPPROVED_STATE_LIBRARY`,
`NO_DIRECT_ZUSTAND_OUTSIDE_STATE_PACKAGE`, `NO_DIRECT_ENV_ACCESS`,
`NO_PROVIDER_SDK_OUTSIDE_INFRA`, `NO_TODO_WITHOUT_ISSUE`.

Structural fields are now emitted by the TS/JS frontend (`curly_depth`,
`round_depth`, `bracket_depth`, `range_lines`, `text_len`, `function`), so the
`.gt/.lt/.ge/.le` comparison and depth predicates work (§8.8).

Deferred to later phases (not in this cut): Python/Go/Rust frontends; `--diff`
mode and caching; the interactive `query` selector; SARIF; and the MCP server.
The Turing-complete scripting tier (Cobra tier 5) is a deliberate non-goal
(proposal §9.5) — stateful whole-program analysis belongs to type-checkers.

## Build

```bash
cargo build --release         # binary at target/release/codepolicy
cargo test                    # unit + integration tests
cargo clippy --workspace --all-targets
```

## Usage

```bash
codepolicy init                       # write a starter codepolicy.yaml
codepolicy init --format rules        # ...or a codepolicy.rules (textual DSL)
codepolicy check                      # check the current repo
codepolicy check apps/ --format agent # LLM-friendly output for a subtree
codepolicy check --rules my.yaml --format json
codepolicy events path/to/File.tsx    # dump the canonical event stream
codepolicy explain-rule NO_DIRECT_GRAPHQL_CLIENT
```

`check` exits `1` when any **error**-severity violation is found (warnings do
not fail the run), `0` when clean, and `2` on a usage/IO error — suitable as a
CI gate or an agent feedback loop.

## Rule DSL (quick reference)

```yaml
rules:
  - id: NO_DIRECT_GRAPHQL_CLIENT
    severity: error            # error | warning
    description: Feature code must use the approved GraphQL access layer.
    applies_to:
      languages: [typescript, javascript]
      paths:
        include: ["apps/*/src/**/*.{ts,tsx,js,jsx}", "packages/**/*.{ts,tsx,js,jsx}"]
        exclude: ["packages/graphql/**"]
    match:
      event: Import
      attrs:
        source: "@apollo/client"                 # scalar equality
        symbols.any: ["useQuery", "gql"]         # list/membership overlap
    message: Use @app/graphql generated hooks instead.
```

**Attribute path mini-language** (suffix operators on an attribute name):

| Form                    | Meaning                                                       |
| ----------------------- | ------------------------------------------------------------ |
| `attr: v`               | the attribute's string forms contain `v`                     |
| `attr.any: [..]`        | the attribute overlaps the listed values                     |
| `attr.regex: "p"`       | some string form matches regex `p`                           |
| `attr.any.regex: "p"`   | some element of a list attribute matches `p`                 |
| `attr.not.regex: "p"`   | **no** string form matches `p` (negation; used by TODO rule) |
| `attr.gt/.lt/.ge/.le: n`| numeric comparison of a scalar attribute                     |
| `attr.eq_ref: var`      | equals a value captured earlier with `bind` (sequences only) |
| `attr.ne_ref: var`      | differs from a captured value (sequences only)               |

**Sequence rules** (`match_sequence`) match an ordered run of events — Cobra
declarative tiers 1–2. A match describes the *violating* shape, so "a
transaction opened with `begin()` and never committed/rolled back in its scope"
is written as the sequence that succeeds when the closer is absent:

```yaml
rules:
  - id: TXN_WITHOUT_COMMIT_OR_ROLLBACK
    severity: warning
    match_sequence:
      anchor: { within: ScopeStart..ScopeEnd }   # one enclosing {} block
      steps:
        - { event: Call, attrs: { name: "begin" }, bind: { tx: receiver } }
        - alt:                                    # commit OR rollback ...
            - [ { event: Call, attrs: { name: "commit",   receiver.eq_ref: tx } } ]
            - [ { event: Call, attrs: { name: "rollback", receiver.eq_ref: tx } } ]
          negate: true                            # ... must NOT appear
          quant: zero_or_more
    message: Transaction has no commit()/rollback() in this scope.
```

Step fields: `event` (`Any` = wildcard), `attrs`, `quant`
(`one`/`optional`/`zero_or_more`/`one_or_more`), `negate`, `bind`, `alt`.
The matcher tries every start position, so the first step may begin anywhere in
the scope and **every** occurrence is reported. `bind` can capture several
attributes at once (e.g. `bind: { obj: receiver, ev: arg }`) to correlate on a
composite key. Member-call events carry a `receiver` attribute (the object
before the method), so a rule can key on *same object + same event*.

**Scope-relative predicates** (`where_scope` on a single-event rule) ask about
the matched event's enclosing `{}` scope without writing a full sequence:

```yaml
match: { event: Call, attrs: { name.regex: "acquire$" } }
where_scope:
  not_contains: { event: Call, attrs: { name.regex: "release$" } }
# clauses: contains | not_contains | followed_by (after the event, before scope end)
```

**Rule composition & cardinality** run as a post-pass over other rules' violations:

```yaml
compose: { op: intersection, of: [RULE_A, RULE_B], key: [file, function] }  # set algebra
count:   { rule: RULE_A, scope: file, op: gt, n: 10 }                       # cardinality
# compose op: intersection | union | difference   ·   count op: gt|lt|ge|le|eq
```

**`unless` guards** (suppress a candidate violation):

| Guard                             | Meaning                                                   |
| --------------------------------- | --------------------------------------------------------- |
| `path.matches: [globs]`           | the file matches one of the globs                         |
| `adr.exists: { topic: "..." }`    | an accepted ADR with that topic exists (repo-wide)        |

In addition, **structured waivers are a global escape hatch**: a waiver naming a
rule and file suppresses that rule there, regardless of `unless`.

## Textual rule syntax (alternative to YAML)

Rules can be written in a concise, Cobra-flavored grammar instead of YAML. Point
`--rules` at a file whose extension is **not** `.yaml`/`.yml` (e.g.
`codepolicy.rules`); `check` also auto-discovers `codepolicy.rules`. It compiles
to the same rules as the YAML form (there's an equivalence test).

```
rule NO_MANUAL_GRAPHQL_OPERATION_TYPES (error) {
  lang typescript
  in "apps/*/src/**/*.{ts,tsx}"
  not in "**/generated/**", "**/*.generated.ts"
  match TypeDecl[name ~ /.*(Query|Mutation|Subscription)(Variables)?$/]
  message "Use generated GraphQL types."
}

rule EVENT_LISTENER_LEAK (warning) {
  lang typescript, javascript
  sequence in scope {
    Call[name="addEventListener"] as obj=receiver, ev=string_args
    not Call[name="removeEventListener", receiver==$obj, string_args==$ev] *
  }
  message "addEventListener with no matching removeEventListener (same object + event)."
}
```

**Predicates** inside `Kind[ … ]`:

| Syntax                        | Meaning                          | YAML key                  |
| ----------------------------- | -------------------------------- | ------------------------- |
| `attr = "v"` / `attr = 5`     | equality                         | `attr`                    |
| `attr ~ /re/`                 | regex                            | `attr.regex`              |
| `attr !~ /re/`                | negated regex                    | `attr.not.regex`          |
| `attr in ["a","b"]`           | membership                       | `attr.any`                |
| `attr > 5` (`< >= <=`)        | numeric comparison               | `attr.gt` / `.lt` / …     |
| `attr == $v` / `attr != $v`   | equals / differs from a binding  | `attr.eq_ref` / `.ne_ref` |

**Bodies** (one per rule):
- `match <pat>` — single event; optionally `where scope contains | not contains | followed by <pat>`
- `sequence [in scope] { <step>… }` — a step may carry `not`, a quantifier (`*` `+` `?`), `as v=attr, …` bindings, and alternation `( a | b )`; `any` is the wildcard kind
- `compose intersection | union | difference of A, B [by file, function]`
- `count RULE per file | function > N`

**Modifiers:** `lang …`; `in "glob"`; `not in "glob"`; `unless path "glob"` / `unless waiver` / `unless adr "topic"`; `message "…"`; `desc "…"`. Globs and string values are double-quoted; regexes are `/…/`; `#` starts a comment.

### Call patterns (positional argument sugar)

Instead of `Call[name="foo", …]`, calls can be written JS-style with positional arguments:

```
foo(a, $x, b)                    # name + 3 positional args (exact arity)
foo(a, $x: string, b)            # 2nd arg must be a string *literal* (syntactic kind)
something(_, _, $third, ..)      # bind the 3rd arg; _ ignores a position; .. ignores the rest
$obj.addEventListener($ev, ..)   # capture the receiver and 1st arg; .. allows more args
el.removeEventListener( .. )     # literal receiver `el`
```

- **Receiver:** `obj.name(…)` (literal) or `$obj.name(…)` (capture/match the object).
- **Arg patterns:** `_` (any), `$var`, a literal (`"str"` / `name` / `5`), each with an optional `: kind`.
- **`..`** as the last argument means "any remaining args" (`arg_count >= N`); without it, arity is exact.
- **Unification:** a `$var`'s first use **binds** it; later uses are **equality** constraints — so `$obj.addEventListener($ev, ..)` … `$obj.removeEventListener($ev, ..)` keys on the *same* object and event with no explicit `as`/`==`.
- **`: kind`** matches the argument's **syntactic kind** — `string`, `template`, `number`, `bool`, `identifier`, `member`, `call`, `object`, `array`, `function`, `regex`. This is the literal/expression form, **not** a static type (that needs the type checker).

These desugar to `argN` / `argN_kind` / `arg_count` predicates over the per-argument metadata the frontend emits. The explicit `Call[…]` form stays available for `callee`, regex names, or non-`Call` kinds.

### Matching arbitrary constructs (the Token layer)

The canonical kinds (`Call`, `Import`, `TypeDecl`, …) are normalized across
languages but don't cover every construct. For anything else — `switch`,
ternaries, decorators, operators, bare symbols — match the generic,
language-local **`Token`** layer by its native `node_kind`:

```
match Token[node_kind = "switch_statement"]
match Token[node_kind = "ternary_expression", curly_depth = 0]
match Token[node_kind = "debugger"]
```

Matchable token fields: `node_kind`, `text`, `function` (enclosing function),
`named` (tree-sitter named node vs. anonymous symbol), `curly_depth` /
`round_depth` / `bracket_depth`, `range_lines`, `text_len`.

`codepolicy events <file> --tokens` lists the available `node_kind`s. This is
Cobra-style universal matching, with the canonical events sitting on top as the
cross-language layer.

Internally the token layer is a compact, columnar stream (`TokenStream`): one
`Copy` `Token` per node, all strings interned per file, prev/next implicit via
index, and a `jmp` link to each construct's matching delimiter — Cobra's
linked-list-of-tokens design, sized to run over millions of lines. Canonical
events stay fat (there are few); tokens go compact (there are many). The stream
is built **only when a rule references `Token`**, so canonical-only checks pay
nothing for it. Token rules are single-event today (`match Token[...]`);
sequences and `where_scope` over tokens are rejected at compile time.

## Escape hatches

- **Waivers** — `*.yaml` under `.codepolicy/waivers/` (configurable via
  `waivers_dir`), keyed on `rule` + `file`. Narrow, file-scoped exceptions.
- **ADRs** — `*.yaml` under `docs/adr/` (configurable via `adr_dir`), with
  `topic` and `status: accepted`. Repo-wide decisions matched by `adr.exists`.

## Architecture (Cargo workspace)

| Crate                   | Responsibility                                            |
| ----------------------- | --------------------------------------------------------- |
| `codepolicy-events`     | Canonical `Event` / `EventKind` / `Span` / `Language`; compact `Token` / `TokenStream` / `Interner` |
| `codepolicy-frontends`  | `LanguageFrontend` (universal token stream + optional canonical overlay); tree-sitter TS/JS + manifest |
| `codepolicy-rules`      | YAML rule schema, compilation, attribute mini-language    |
| `codepolicy-match`      | Event indexing, matching, `unless`/waiver evaluation      |
| `codepolicy-report`     | `human` / `json` / `agent` rendering                      |
| `codepolicy-core`       | Discovery, parallel extraction, the check pipeline        |
| `codepolicy-cli`        | The `codepolicy` binary; bundled starter rules; fixtures  |

## Adding a language frontend

A frontend turns one source file into up to two streams. The whole contract is
one trait in `codepolicy-frontends`:

```rust
pub struct SourceFile<'a> { pub path: Utf8PathBuf, pub text: &'a str }

#[derive(Default)]
pub struct Extracted {
    pub tokens: Option<TokenStream>, // universal, compact token layer (Cobra-style)
    pub events: Vec<Event>,          // optional canonical overlay (Import, Call, …)
}

pub trait LanguageFrontend: Sync {
    fn name(&self) -> &'static str;                 // for diagnostics
    fn supports_file(&self, path: &Utf8Path) -> bool;
    fn extract(&self, file: &SourceFile<'_>, want_tokens: bool) -> anyhow::Result<Extracted>;
}
```

Two output streams, two jobs:

- **Token stream** (`Extracted.tokens`) — the universal primary layer. Fill it
  only when `want_tokens` is true (the pipeline sets that only if a loaded rule
  references `Token`, so canonical-only runs pay nothing). Producible by a plain
  **lexer** — no parse tree required. This is what `match Token[node_kind = …]`
  rules see.
- **Canonical events** (`Extracted.events`) — the optional, normalized,
  cross-language overlay (`Import`, `Call`, `EnvAccess`, `TypeDecl`, …). This is
  what every non-`Token` rule matches. A lexer-only frontend may leave it empty;
  a parser-based frontend projects its tree down to these kinds.

So a frontend can be **lexer-only** (tokens, no events), **structured-only**
(events, no tokens — like the `package.json` manifest frontend), or **both**
from a single parse (like the TS/JS frontend).

### Steps

1. **Register the `Language`** (`codepolicy-events`): if your language isn't
   already in the `Language` enum, add a variant and a `from_extension` arm.
   (`Go`, `Python`, `Rust` are already listed — a frontend is all that's
   missing for those.) The lowercase serde name is what rule files match in
   `lang <name>`.
2. **Add a module** under `codepolicy-frontends/src/` and implement
   `LanguageFrontend`. `supports_file` keys off the extension; `extract` builds
   `Extracted`. If you use tree-sitter, add the grammar crate to that crate's
   `Cargo.toml` (see how `tree-sitter-typescript` / `tree-sitter-javascript` are
   wired in `ts_js.rs`).
3. **Register it** in `default_frontends()` (`codepolicy-frontends/src/lib.rs`).
   `frontend_for` returns the first frontend whose `supports_file` matches, so
   order matters if extensions overlap.
4. **Add a fixture** under `fixtures/` (the workspace root, where the existing
   `repo`/`token_repo`/… live) and an assertion in
   `crates/codepolicy-cli/tests/fixtures.rs` so the new language's
   events/tokens are pinned.

### Conventions to honor

- **Spans** are 1-based for line/column and **end-exclusive**; byte offsets are
  0-based. (`ts_js.rs::span_of` converts tree-sitter's 0-based rows.)
- **Set `language`** on both `Event` and `TokenStream` to your `Language` so
  `lang`-scoped rules apply correctly.
- **Share the file path** as one `Arc<Utf8PathBuf>` across all of a file's
  events/tokens (`Event::new` takes `impl Into<Arc<…>>`) — emitting one token
  per node must not reclone the path.
- **Reuse the canonical attribute vocabulary** so existing rules work
  unchanged: e.g. `Import` carries `source` + `symbols`; `Call` carries `name`,
  `callee`, `receiver`, `string_args`, `arg_count`, and per-position
  `argN`/`argN_kind`; `TypeDecl` carries `decl_kind` + `name`. Attach the
  reserved structural fields (`curly_depth`, `round_depth`, `bracket_depth`,
  `range_lines`, `text_len`, `function`) where you can — see
  `attach_structural`. `argN_kind` is a **syntactic** kind (the literal/
  expression form), not a static type.
- **For the token layer**, intern every string (`Interner`), set the nesting
  depths and enclosing `func` per token, and pair delimiters into `jmp` links
  (the `TokenBuilder::push` / `finish_jmp` pattern in `ts_js.rs`).

The two reference frontends are the templates: `ts_js.rs` (tree-sitter,
both streams from one walk) and `manifest.rs` (structured-only, ~60 lines).

## Tests

- Per-crate unit tests (event serde, rule compilation, matching, reporting).
- `crates/codepolicy-cli/tests/fixtures.rs`: end-to-end runs over
  `fixtures/repo` (asserts the exact violation set, including exempt paths) and
  `fixtures/repo_waived` (asserts ADR + waiver suppression).
