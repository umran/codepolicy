# Cobra-Inspired Multi-Language Policy Harness for LLM-Assisted Software Engineering

**Working name:** `harness` / `codepolicy` / `cobra-lite`  
**Primary implementation language:** Rust  
**Target languages:** TypeScript/JavaScript, Go, Python, Rust  
**Primary use case:** Fast, verifiable, repository-local software engineering policy enforcement for humans and LLM coding agents.

---

## 1. Purpose

This document proposes a preliminary design for a fast, multi-language, event-stream-based static policy tool inspired by NASA/JPL's Cobra static analyzer. The substrate is an *event* stream rather than a raw token stream: although Cobra itself operates on a lexical token list, this tool derives a flat, language-neutral event stream by walking each language's parse tree (see §6 and §10.5).

The goal is to build a tool that can enforce project-specific software engineering practices without requiring heavyweight semantic analysis or bespoke AST walkers for every rule.

The tool should help a coding agent stay within the intended engineering style of a project by making key practices mechanically checkable.

Examples of policies this tool should enforce:

- Frontend feature code must not import low-level GraphQL clients directly.
- GraphQL access must go through generated types/documents/hooks.
- Cross-component client state must use the approved state layer, such as Zustand behind `@app/state`.
- Environment variables must be read only through the typed configuration layer.
- Backend code must not import provider SDKs outside designated infrastructure packages.
- Generated files must not be hand-edited.
- New dependencies must be approved, justified, or covered by an ADR/waiver.

This is not intended to replace compilers, typecheckers, linters, tests, or full static analyzers. It is intended to fill the gap between:

```text
too weak: natural-language guidance only
too heavy: custom AST/dataflow analysis for every project-specific rule
```

The core idea is:

```text
source files
  -> language-specific lexical/parsing frontend
  -> shared canonical token/event stream
  -> declarative pattern/sequence rules (regular core + capture + counting)
  -> precise violations with file/line evidence
  -> CI/LLM feedback loop
```

---

## 2. Inspiration: Cobra and the JPL Style of Lightweight Static Analysis

### 2.1 Cobra overview

Cobra is a fast static source-code analyzer originally developed at NASA/JPL. Its public repository describes it as a tool for interactively probing and querying up to millions of lines of code, with a basic design that is language-neutral even though many included rule libraries target C or C-like languages.[^cobra-github]

The key design lesson is that many useful code checks do not require full parsing, type inference, alias analysis, control-flow graphs, or data-flow analysis. Many project rules can be checked by operating over lexical tokens plus a modest amount of structural annotation.

Space ROS's Cobra documentation summarizes the design this way: Cobra first performs lexical analysis to generate a stream of language-level tokens, stores them in a simple data structure, and then applies rule sets to search for patterns indicating violations.[^space-ros-cobra]

The Cobra reference manual describes its internal representation as a basic linked list of lexical tokens, annotated with information and links to matching parentheses, brackets, and braces.[^cobra-manual]

### 2.2 Why token-level matching is valuable

Raw text search is too imprecise. For example, `grep x` will match `x` inside identifiers, comments, and string literals. Cobra-style token search avoids that by matching actual lexical tokens rather than raw substrings. Cobra's pattern-search docs illustrate this by contrasting grep with token-level matching, where a token pattern for `x` avoids matching identifiers like `prefix` or string literals such as `"x"`.[^cobra-patterns]

This is important because project-specific engineering policies often need lexical precision without full semantic understanding.

### 2.3 What we want to borrow

The proposed tool borrows the following ideas from Cobra:

1. **Operate primarily over a flat event stream.** Derive a flat, language-neutral event stream from each frontend (lexically, or by walking a parse tree) and avoid heavyweight whole-program analysis unless a rule truly requires it.
2. **Make pattern rules cheap to execute.** Compile rules into deterministic matchers over normalized events/tokens.
3. **Keep rules small and inspectable.** Prefer many simple checks over a few magical semantic analyses.
4. **Return concrete evidence.** Every violation should point to a file, line, span, matched token/event, and rule ID.
5. **Scale to large repositories.** Use streaming, indexing, caching, and incremental re-checking.
6. **Support interactive exploration later.** The first version can be CI-focused, but the architecture should permit interactive querying.

---

## 3. Problem Statement

LLM coding agents are good at producing code, but they drift unless project constraints are made explicit and mechanically enforced.

Natural-language rules such as:

> Use generated GraphQL types instead of hand-rolling types.

or:

> Prefer Zustand for human-maintainable frontend state.

are helpful for prompting but not sufficient for long-term codebase governance. The agent may follow them once, forget them later, or satisfy them superficially.

Policy engines such as OPA/Rego can evaluate structured facts, but they do not themselves know how to extract accurate facts from code.

Full AST/dataflow analysis can extract richer facts, but building bespoke analyzers for every rule and every language is not scalable.

The proposed solution is a middle layer:

> Generate a shared, language-neutral stream of canonical code events from language-specific source files, then apply declarative pattern rules over those events.

The rule language is kept deliberately bounded (a regular core extended with capture and counting; see §9.4) and functionally equivalent to Cobra's declarative pattern and set language (§9.5) — without adopting Cobra's Turing-complete inline-scripting tier.

---

## 4. Core Design Principle

The tool should not try to understand arbitrary code perfectly.

Instead:

> Design the codebase so the facts that matter are visible at stable choke points, then build fast token/event checks around those choke points.

Examples:

- State policy should be visible through dependencies, imports, and approved state wrappers.
- GraphQL policy should be visible through operation files, generated artifacts, approved hooks, and forbidden raw client imports.
- Configuration policy should be visible through environment access events.
- Database policy should be visible through imports, migration files, and approved repository modules.
- Provider integration policy should be visible through SDK imports and approved infrastructure paths.

The tool should make forbidden patterns and required pathways mechanically visible, surfacing the violations it can recognize at the chosen lexical and event-level choke points. It does **not** prove the *absence* of all forbidden behavior: patterns it cannot observe — import aliasing, dynamic or reflective access, indirection, re-exports, macro expansion — may evade it. Its value is high-precision detection of casual violations, not soundness. It also does not need to prove that every design choice is ideal.

---

## 5. High-Level Architecture

```text
                       +-----------------------+
                       |       Source Repo     |
                       +-----------+-----------+
                                   |
                                   v
                       +-----------------------+
                       | File discovery / diff |
                       +-----------+-----------+
                                   |
                +------------------+------------------+
                |                  |                  |
                v                  v                  v
        +---------------+  +---------------+  +---------------+
        | TS/JS frontend|  | Go frontend   |  | Python frontend|
        +-------+-------+  +-------+-------+  +-------+-------+
                |                  |                  |
                +------------------+------------------+
                                   |
                                   v
                       +-----------------------+
                       | Canonical event stream|
                       +-----------+-----------+
                                   |
                                   v
                       +-----------------------+
                       | Event indexes/cache   |
                       +-----------+-----------+
                                   |
                                   v
                       +-----------------------+
                       | Pattern rule engine   |
                       +-----------+-----------+
                                   |
                                   v
                       +-----------------------+
                       | Violations / warnings |
                       +-----------+-----------+
                                   |
                                   v
                       +-----------------------+
                       | CI / OPA / LLM output |
                       +-----------------------+
```

Rust frontend support is added alongside Python and Go in v0 (see §11.1 and §17 Phase 5).

---

## 6. Language Frontends

The tool should have language-specific frontends, but those frontends should emit the same shared event vocabulary wherever possible.

The per-language names listed below are **language-local node kinds** (shown in `SCREAMING_CASE`). Each frontend normalizes them into the canonical `EventKind` vocabulary of §7.2 before any rule runs; see §7.2.1 for the normalization mapping.

### 6.1 TypeScript / JavaScript

Recommended implementation approach:

- Use Tree-sitter grammars for JavaScript, TypeScript, and TSX initially.
- Alternatively, use the TypeScript compiler API for higher-fidelity TS-specific metadata later.
- Emit imports, calls, type declarations, JSX elements, comments, string literals, and environment access events.

Useful emitted events:

```text
IMPORT
EXPORT
CALL
HOOK_CALL
TYPE_DECL
FUNC_DECL
CLASS_DECL
JSX_ELEMENT
ENV_ACCESS
STRING_LITERAL
COMMENT
GRAPHQL_OPERATION
```

Important TS/JS patterns:

- Direct `@apollo/client` imports in feature code.
- Raw `fetch('/graphql')` calls.
- Manual `*Query`, `*Mutation`, or `*Variables` type declarations outside generated paths.
- Direct `process.env.X` access outside config.
- Forbidden state-management imports.
- Direct provider SDK imports outside infrastructure packages.

### 6.2 Go

Recommended implementation approach:

- Use Go's standard parser/scanner or Tree-sitter Go.
- Prefer standard Go tooling if the Rust implementation can invoke or reuse reliable Go parsing indirectly; otherwise Tree-sitter is acceptable for the first pass.

Useful emitted events:

```text
PACKAGE_DECL
IMPORT
CALL
FUNC_DECL
METHOD_DECL
TYPE_DECL
STRUCT_DECL
INTERFACE_DECL
ENV_ACCESS
GOROUTINE_START
CHANNEL_OP
COMMENT
```

Important Go patterns:

- `os.Getenv` outside config package.
- Direct database/client imports outside infrastructure/repository packages.
- `context.TODO()` in production handlers.
- `http.Client` or external requests without timeout policy.
- Provider SDK imports outside designated packages.

### 6.3 Python

Recommended implementation approach:

- Use Tree-sitter Python for consistency, or Python's built-in tokenizer/AST through a helper if needed.
- Preserve indentation/scope structure as logical events.

Useful emitted events:

```text
IMPORT
FROM_IMPORT
CALL
FUNC_DECL
CLASS_DECL
DECORATOR
TYPE_DECL
ENV_ACCESS
WITH_BLOCK
EXCEPT_BLOCK
COMMENT
STRING_LITERAL
```

Important Python patterns:

- `os.getenv` / `os.environ[...]` outside config.
- `requests.get/post/...` without timeout.
- Bare `except:`.
- Direct SDK imports outside infrastructure modules.
- Print statements in server/runtime code instead of approved logging.

### 6.4 Rust

Recommended implementation approach:

- Use Tree-sitter Rust for the first implementation.
- Later consider `syn`, `proc_macro2`, or `rustc_lexer` if richer Rust-specific extraction is needed.
- Treat macros carefully: detect macro calls, but do not assume expansion unless an explicit macro-expansion step is added.

Useful emitted events:

```text
USE
IMPORT
CALL
MACRO_CALL
FUNC_DECL
STRUCT_DECL
ENUM_DECL
TRAIT_DECL
IMPL_BLOCK
ENV_ACCESS
UNSAFE_BLOCK
ATTRIBUTE
COMMENT
STRING_LITERAL
```

Important Rust patterns:

- `std::env::var` outside config modules.
- `unsafe` blocks without an approved comment/waiver.
- Direct SQL/HTTP/provider client usage outside approved modules.
- `unwrap()` / `expect()` in production paths if project policy forbids them.
- Macro calls to known risky APIs.

---

## 7. Canonical Event Model

The event model is the heart of the tool.

The goal is not to preserve every syntactic detail. The goal is to preserve the facts that project policies care about.

### 7.1 Event envelope

All events should share a common envelope:

```rust
use camino::Utf8PathBuf;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Event {
    pub kind: EventKind,
    pub language: Language,
    pub file: Utf8PathBuf,
    pub span: Span,
    pub attrs: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Language {
    Typescript,
    Javascript,
    Go,
    Python,
    Rust,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Span {
    // 0-based byte offsets; `end_byte` is exclusive. Omitted from the JSON
    // examples below for brevity, hence `#[serde(default)]`.
    #[serde(default)]
    pub start_byte: usize,
    #[serde(default)]
    pub end_byte: usize,
    // 1-based line and column numbers; `end_line`/`end_col` are exclusive.
    pub start_line: usize,
    pub start_col: usize,
    pub end_line: usize,
    pub end_col: usize,
}
```

This block assumes `camino = { version = "1", features = ["serde1"] }` (the `serde1` feature is required for `Utf8PathBuf` to implement `Serialize`/`Deserialize`) and `serde_json` for the `Value` used in `attrs`.

**Span conventions.** Lines and columns are **1-based**; `end_line`, `end_col`, and `end_byte` are **exclusive** (spans are half-open). `start_byte`/`end_byte` are **0-based** byte offsets. Tree-sitter's native `start_point`/`end_point` are 0-based for both row and column, so a Tree-sitter frontend must add 1 to row and column when emitting a `Span`. The byte fields are left out of the JSON examples below purely for readability.

`attrs` keeps v0 flexible. Later, frequently used events can gain typed structs for speed and safety.

### 7.2 Core event kinds

Initial event vocabulary:

```rust
/// Canonical event vocabulary. Uses serde's default externally-tagged
/// representation, so the JSON `kind` value is the exact PascalCase variant
/// name (e.g. `EnvAccess` -> "EnvAccess"). Do NOT add
/// `#[serde(rename_all = ...)]` here, or it will diverge from the examples
/// below. (This is intentionally the opposite of `Language`, which is renamed
/// to lowercase.)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum EventKind {
    File,
    PackageAdded,
    Import,
    Export,
    Call,
    NewExpr,
    TypeDecl,
    FunctionDecl,
    MethodDecl,
    ClassDecl,
    Attribute,
    EnvAccess,
    StringLiteral,
    Comment,
    ScopeStart,
    ScopeEnd,
    GraphqlOperation,
    GeneratedFile,
    ConfigFile,
    Token, // generic, language-local node (Cobra-style); see §7.2.1
}
```

### 7.2.1 Normalizing language-local kinds to canonical events

Each frontend emits **language-local node kinds** (the `SCREAMING_CASE` names in §6) and normalizes them into the canonical `EventKind` vocabulary above before any rule runs. A representative (non-exhaustive) mapping:

| Language-local kind(s)                                                  | Canonical `EventKind`     |
| ---------------------------------------------------------------------- | ------------------------- |
| `IMPORT`, `USE`, `FROM_IMPORT`, `PACKAGE_DECL`                          | `Import`                  |
| `EXPORT`                                                                | `Export`                  |
| `CALL`, `HOOK_CALL`, `MACRO_CALL`, `CHANNEL_OP`, `GOROUTINE_START`      | `Call`                    |
| `NEW_EXPR`, `JSX_ELEMENT`                                               | `NewExpr`                 |
| `TYPE_DECL`, `STRUCT_DECL`, `INTERFACE_DECL`, `ENUM_DECL`, `TRAIT_DECL` | `TypeDecl`                |
| `FUNC_DECL`                                                             | `FunctionDecl`            |
| `METHOD_DECL`                                                           | `MethodDecl`              |
| `CLASS_DECL`                                                            | `ClassDecl`               |
| `ATTRIBUTE`, `DECORATOR`                                                | `Attribute`               |
| `WITH_BLOCK`, `EXCEPT_BLOCK`, `IMPL_BLOCK`, `UNSAFE_BLOCK`              | `ScopeStart` / `ScopeEnd` |
| `ENV_ACCESS`                                                            | `EnvAccess`               |
| `STRING_LITERAL`                                                        | `StringLiteral`           |
| `COMMENT`                                                               | `Comment`                 |
| `GRAPHQL_OPERATION`                                                     | `GraphqlOperation`        |

For language-specific precision beyond the canonical kinds, a frontend also emits a generic **`Token`** event for *every* node (Cobra-style), carrying its native `node_kind` (e.g. `switch_statement`) and `text`. This universal layer makes any construct matchable — `Token[node_kind="switch_statement"]` — while the canonical kinds above remain the normalized, cross-language views layered on top. The `Token` stream is emitted only when a loaded rule references it, so canonical-only runs pay nothing for it. `PackageAdded`, `GeneratedFile`, and `ConfigFile` are **not** produced by source frontends; `PackageAdded` comes from a manifest/VCS frontend (see §11.3), and the two `*File` kinds are synthesized during file discovery.

In addition to language-specific attributes, every event carries a small set of **reserved structural attributes** computed from the parse tree — `curly_depth`, `round_depth`, `bracket_depth`, `range_lines`, `text_len`, and `function` (the enclosing function name) — mirroring Cobra's precomputed `.curly`/`.round`/`.bracket`/`.range`/`.len`/`.fct` fields. Rules consume them through the comparison operators of §8.8. They are reserved (frontends must not use these keys for anything else) and optional in v0 (a frontend that does not compute a field simply omits it).

`Call` events additionally carry **positional-argument attributes**: `arg_count`, and for each argument `argN` (its value/text) and `argN_kind` (its *syntactic* kind — `string`, `number`, `identifier`, `call`, … — not a static type). These let rules match or capture a specific argument by position (e.g. bind the 3rd argument while ignoring the rest), and underpin the call-pattern surface syntax `name(a, $x: string, ..)`.

### 7.3 Example events

TypeScript:

```ts
import { useQuery } from "@apollo/client";
fetch('/graphql');
interface GetMemberQuery { member: { id: string } }
```

Canonical events:

```json
[
  {
    "kind": "Import",
    "language": "typescript",
    "file": "apps/admin/src/Member.tsx",
    "span": { "start_line": 1, "start_col": 1, "end_line": 1, "end_col": 43 },
    "attrs": {
      "source": "@apollo/client",
      "symbols": ["useQuery"]
    }
  },
  {
    "kind": "Call",
    "language": "typescript",
    "file": "apps/admin/src/Member.tsx",
    "span": { "start_line": 2, "start_col": 1, "end_line": 2, "end_col": 19 },
    "attrs": {
      "name": "fetch",
      "string_args": ["/graphql"]
    }
  },
  {
    "kind": "TypeDecl",
    "language": "typescript",
    "file": "apps/admin/src/Member.tsx",
    "span": { "start_line": 3, "start_col": 1, "end_line": 3, "end_col": 52 },
    "attrs": {
      "decl_kind": "interface",
      "name": "GetMemberQuery"
    }
  }
]
```

Go:

```go
import "os"
key := os.Getenv("API_KEY")
```

Canonical event:

```json
{
  "kind": "EnvAccess",
  "language": "go",
  "file": "internal/api/handler.go",
  "span": { "start_line": 2, "start_col": 8, "end_line": 2, "end_col": 28 },
  "attrs": {
    "name": "API_KEY",
    "via": "os.Getenv"
  }
}
```

Python:

```python
import os
key = os.getenv("API_KEY")
```

Canonical event:

```json
{
  "kind": "EnvAccess",
  "language": "python",
  "file": "app/handler.py",
  "span": { "start_line": 2, "start_col": 7, "end_line": 2, "end_col": 27 },
  "attrs": {
    "name": "API_KEY",
    "via": "os.getenv"
  }
}
```

Rust:

```rust
let key = std::env::var("API_KEY")?;
```

Canonical event:

```json
{
  "kind": "EnvAccess",
  "language": "rust",
  "file": "src/handler.rs",
  "span": { "start_line": 1, "start_col": 11, "end_line": 1, "end_col": 35 },
  "attrs": {
    "name": "API_KEY",
    "via": "std::env::var"
  }
}
```

The same cross-language rule catches all three — **provided each frontend normalizes its environment accessors into `EnvAccess` events.** The examples above show only the direct call forms. Frontends must also emit `EnvAccess` for the other accessor shapes, or those will silently escape the rule:

- Python: `os.getenv(...)`, `os.environ[...]` (a subscript, not a call), `os.environ.get(...)`
- Go: `os.Getenv(...)`, `os.LookupEnv(...)`
- Rust: `std::env::var(...)`, `std::env::var_os(...)`, `std::env::vars()`
- TS/JS: `process.env.X`, `process.env[name]`, and destructuring of `process.env`

---

## 8. Rule Language

The rule language should be declarative YAML/TOML/JSON. YAML is friendlier for hand-written rules; TOML may feel more Rust-native. Choose one for v0 and support the other later only if needed. An implementation may additionally offer a concise, Cobra-flavored *textual* grammar (e.g. `Kind[attr ~ /re/]`, `sequence in scope { … }`) that parses to the same rule structures — a more ergonomic surface than YAML for hand-authoring, with no change to the engine.

The rule language should be intentionally small. It should not become a general-purpose programming language. It grows (in §8.6–8.10) to match the expressiveness of Cobra's declarative pattern and set language — sequences, captures, scope-relative predicates, composition, and counting — but it stays **declarative and non-Turing-complete**: every rule terminates in time linear in the event stream (§9.4). The stateful, unbounded analyses that Cobra reserves for its inline-script tier are a deliberate non-goal here (§9.5, §11.4).

### 8.1 Design goals

Rules should be:

- Small.
- Reviewable.
- Version-controlled.
- Easy for an LLM to propose but easy for a human to inspect.
- Compilable into efficient matchers.
- Able to produce precise file/line violations.

### 8.2 Single-event rule

```yaml
rules:
  - id: NO_DIRECT_GRAPHQL_CLIENT
    severity: error
    description: Feature code must use the approved GraphQL access layer.
    applies_to:
      languages: [typescript, javascript]
      paths:
        include:
          - "apps/*/src/**/*.{ts,tsx,js,jsx}"
          - "packages/**/*.{ts,tsx,js,jsx}"
        exclude: ["packages/graphql/**"]
    match:
      event: Import
      attrs:
        source: "@apollo/client"
        symbols.any: ["useQuery", "useMutation", "gql", "ApolloClient"]
    message: "Use @app/graphql generated hooks instead of importing Apollo directly."
```

### 8.3 Regex attribute rule

```yaml
rules:
  - id: NO_MANUAL_GRAPHQL_OPERATION_TYPES
    severity: error
    description: GraphQL operation types must be generated, not hand-written.
    applies_to:
      languages: [typescript]
      paths:
        include: ["apps/*/src/**/*.{ts,tsx}"]
        exclude: ["**/generated/**", "**/*.generated.ts"]
    match:
      event: TypeDecl
      attrs:
        name.regex: ".*(Query|Mutation|Subscription)(Variables)?$"
    message: "Do not hand-write GraphQL operation types. Use generated types."
```

> **Caveat — heuristic.** This name-suffix regex also matches unrelated domain types such as `SearchQuery`, `DatabaseQuery`, or `EventSubscription`. To keep false positives low, pair it with a corroborating GraphQL signal in the same file (a `gql`/`GraphqlOperation` event, or a GraphQL-client import) or run it as a `warning` with an allowlist. The example keeps `severity: error` only for illustration.

### 8.4 Cross-language environment rule

```yaml
rules:
  - id: NO_DIRECT_ENV_ACCESS
    severity: error
    description: Environment variables must be read only through the project config layer.
    applies_to:
      languages: [typescript, javascript, go, python, rust]
      paths:
        include: ["**/*"]
        exclude: ["**/config/**", "**/settings/**"]
    match:
      event: EnvAccess
    message: "Read environment variables through the typed config layer."
```

> **Caveat — illustrative globs.** `**/config/**` and `**/settings/**` are blunt: they exempt *any* directory named `config`/`settings` anywhere in the tree (over-matching), while missing a config layer that lives in a single file such as `src/config.ts` (under-matching). Configure explicit, anchored paths per project (e.g. `packages/config/**`, `src/config.ts`) rather than relying on these defaults.

### 8.5 Package rule

```yaml
rules:
  - id: NO_UNAPPROVED_STATE_LIBRARY
    severity: error
    description: Frontend state management libraries must be explicitly approved.
    match:
      event: PackageAdded
      attrs:
        name.any:
          - redux
          - "@reduxjs/toolkit"
          - recoil
          - jotai
          - mobx
          - xstate
    unless:
      adr.exists:
        topic: "state management exception"
    message: "Use Zustand/@app/state unless an ADR justifies another state library."
```

### 8.6 Sequence patterns

Single-event rules (§8.2–8.5) match one event. A **sequence pattern** matches an *ordered* run of events, so a rule can say "A, then B, with no C in between." It is evaluated as a finite-state pass (a Thompson NFA) over the event stream of one file, optionally restricted to a single pre-paired scope. The non-regular work — matching nested `{}` — is done upstream by the frontend, which emits paired `ScopeStart`/`ScopeEnd` events; the matcher only walks the flat stream. This mirrors Cobra, which precomputes links to matching delimiters rather than inferring nesting in the matcher.

A `match_sequence` is a list of `steps`. Each step matches one or more consecutive events:

| Step field    | Meaning                                                                              |
| ------------- | ----------------------------------------------------------------------------------- |
| `event`       | event kind to match (`Any` matches any kind — the analog of Cobra's `.`)             |
| `attrs`       | attribute predicates (§9.1.1), as in single-event rules                              |
| `quant`       | `one` (default), `optional`, `zero_or_more`, `one_or_more`                           |
| `negate: true`| the step matches an event that does **not** satisfy the matcher (Cobra's `^`)        |
| `bind`        | capture an attribute value for later backreference (§8.7)                            |
| `alt`         | a list of alternative sub-sequences; the step matches if any alternative does        |

`anchor: { within: ScopeStart..ScopeEnd }` restricts the whole sequence to the events of one enclosing scope. To allow arbitrary gaps between two steps (Cobra's `.*`), insert a wildcard run `{ event: Any, quant: zero_or_more }`.

**A `match_sequence` describes the *violating* shape — a match is a violation.** So "allocation with no `free` before the end of its block" is written as the sequence that succeeds exactly when `free` is absent:

```yaml
rules:
  - id: ALLOC_WITHOUT_FREE_IN_SCOPE
    severity: warning
    applies_to: { languages: [c, rust] }
    match_sequence:
      anchor: { within: ScopeStart..ScopeEnd }
      steps:
        - { event: Call, attrs: { name.regex: "^(malloc|calloc|emalloc)$" } }
        - { event: Call, attrs: { name: "free" }, negate: true, quant: zero_or_more }
    message: "Allocation in this block has no matching free before the block ends."
```

The earlier transaction rule is the same shape with an alternation of acceptable closers. The `start` / `until` / `require_any` "region" form is simply sugar for a sequence whose final step is a negated alternation:

```yaml
rules:
  - id: TRANSACTION_WITHOUT_COMMIT_OR_ROLLBACK
    severity: warning
    description: Transaction-like regions should visibly commit or roll back.
    applies_to: { languages: [go, typescript, python, rust] }
    match_sequence:
      anchor: { within: ScopeStart..ScopeEnd }
      steps:
        - { event: Call, attrs: { name.regex: "(begin|beginTransaction|BeginTx)" } }
        - alt:
            - [ { event: Call, attrs: { name.regex: "(commit|Commit)" } } ]
            - [ { event: Call, attrs: { name.regex: "(rollback|Rollback)" } } ]
          negate: true
          quant: zero_or_more
    message: "Transaction appears to lack a visible commit or rollback."
```

Sequence rules should default to `warning`: even with pre-paired scopes, lexical sequencing cannot see data flow (a `free` reached only on one branch, a commit in a called helper) and can produce false positives.

### 8.7 Capture variables and backreferences

A step may **bind** an attribute value to a name, and a later step may require another event's attribute to equal (`.eq_ref`) or differ from (`.ne_ref`) that bound value. This is Cobra's `x:@ident … :x` — the one feature that takes the language past the regular class — but it is implemented with a small environment of captured *strings*, not with regex backreferences (the `regex` crate still has none; see §9.3).

```yaml
rules:
  - id: LOCK_WITHOUT_MATCHING_UNLOCK
    severity: warning
    applies_to: { languages: [go, rust, c] }
    match_sequence:
      anchor: { within: ScopeStart..ScopeEnd }
      steps:
        - event: Call
          attrs: { name.regex: "(mutex_lock|Lock)$" }
          bind: { lock_obj: receiver }          # capture which object was locked
        - event: Call
          attrs: { name.regex: "(mutex_unlock|Unlock)$", receiver.eq_ref: lock_obj }
          negate: true
          quant: zero_or_more
    message: "Lock acquired on this object is not released on the same object before scope end."
```

`bind` captures the (scalar) value of one or more attributes — `bind: { obj: receiver, ev: arg }` captures two at once — and `eq_ref`/`ne_ref` compare a later event's attribute against a binding. Bindings are scoped to a single sequence match. Multi-binding lets a rule correlate on a *composite* key (e.g. same object **and** same event), not just a single value. A relational constraint like Cobra's `(:x != :z)` is expressed with `.ne_ref` — e.g. "a name is re-declared under a *different* type": bind the first `decl_kind`, then match a same-named `TypeDecl` whose `decl_kind.ne_ref` is that binding.

### 8.8 Structural-field and comparison predicates

Cobra exposes precomputed per-token fields (nesting depth `.curly`/`.round`, range length `.range`, token length `.len`, function context `.fct`) and numeric comparison. The event model reserves the analogous fields, populated by each frontend from the parse tree (§7.2.1), and the attribute mini-language adds comparison operators `.gt` / `.lt` / `.ge` / `.le` (§9.1.1):

| Reserved attribute | Meaning (Cobra analog)                                   |
| ------------------ | ------------------------------------------------------- |
| `curly_depth`      | enclosing `{}` nesting depth (`.curly`)                  |
| `round_depth`      | enclosing `()` nesting depth (`.round`)                  |
| `bracket_depth`    | enclosing `[]` nesting depth (`.bracket`)                |
| `range_lines`      | line count of the event's associated body/range (`.range`) |
| `text_len`         | length of the matched token/text (`.len`)                |
| `function`         | name of the enclosing function (`.fct`)                  |

```yaml
rules:
  - id: LONG_FUNCTION
    severity: warning
    match:
      event: FunctionDecl
      attrs:
        range_lines.gt: 200
    message: "Function body exceeds 200 lines; consider splitting."

  - id: TOP_LEVEL_SIDE_EFFECT
    severity: warning
    applies_to: { languages: [python] }
    match:
      event: Call
      attrs:
        name: "connect"
        curly_depth: 0            # module scope, not inside any block/function
    message: "Side-effecting call at import time."
```

These predicates stay regular — each is a boolean test on one event; the non-regular work (computing nesting and range) is done upstream by the parser, exactly as Cobra precomputes the fields.

### 8.9 Scope-relative predicates

A single-event rule can assert something about the matched event's **enclosing scope** without writing a full sequence, via `where_scope`. From the matched event, the matcher resolves the innermost enclosing `ScopeStart`/`ScopeEnd` pair (falling back to the whole file when nothing encloses it) and evaluates:

| Clause         | Meaning                                                         |
| -------------- | -------------------------------------------------------------- |
| `contains`     | the enclosing scope contains a matching event                   |
| `not_contains` | the enclosing scope contains no matching event                  |
| `followed_by`  | a matching event occurs after the matched event, before scope end |

All present clauses are ANDed. This is the declarative, *enclosing-scope* form of Cobra's `contains`/`extend` filters — "within the block that holds this event, does X (not) appear?" It is deliberately **not** Cobra's arbitrary `.jmp` pointer navigation (jump to a construct's own delimiter and walk from there), which stays in the interactive tier (§9.5):

```yaml
rules:
  - id: ACQUIRE_WITHOUT_RELEASE_IN_SCOPE
    severity: warning
    match:
      event: Call
      attrs: { name.regex: "acquire$" }
    where_scope:
      not_contains:
        event: Call
        attrs: { name.regex: "release$" }
    message: "acquire() in this scope has no matching release()."
```

`where_scope` requires `ScopeStart`/`ScopeEnd` events (post-v0, §11.2).

### 8.10 Rule composition and cardinality

The constructs above match within one file in a single pass. Two further constructs combine the *results* of other rules — Cobra's named-set algebra (`ps C = A & B`) and set cardinality (`size(n) > k`):

- **`compose`** applies set algebra to other rules' violation sets, grouped by a locus key:

```yaml
rules:
  - id: LOOP_THAT_REASSIGNS_AND_ALLOCATES
    compose:
      op: intersection          # intersection | union | difference
      of: [LOOP_REASSIGNS_INDEX, LOOP_CALLS_ALLOC]
      key: [file, function]     # what counts as "the same locus"
    message: "A loop both reassigns its index and allocates."
```

- **`count`** fires when the number of another rule's matches in a scope crosses a threshold:

```yaml
rules:
  - id: TOO_MANY_DIRECT_ENV_ACCESSES
    count:
      rule: NO_DIRECT_ENV_ACCESS
      scope: file                # file | function
      op: gt                     # gt | lt | ge | le | eq
      n: 10
    message: "More than 10 direct env accesses in one file — centralize them in config."
```

Both run as a bounded aggregation pass *after* primary matching. Cardinality comparison is what makes the language formally context-sensitive (§9.4), but the implementation is a counting pass, not a general program.

---

## 9. Pattern Engine

### 9.1 Matching model

The rule engine should operate over canonical events, not raw source text.

For v0, support:

- Event kind matching.
- Attribute equality.
- Attribute list membership.
- Attribute regex.
- Attribute regex over list elements (`attr.any.regex`).
- Path glob include/exclude.
- Language include/exclude.
- Simple `unless` clauses (path, `waiver.exists`, `adr.exists`).
- Single-event rules.

Beyond v0, the full proposed expressiveness — functionally equivalent to Cobra's declarative pattern and set language (§9.5) — adds:

- Numeric comparison operators (`attr.gt/.lt/.ge/.le`) and reserved structural fields (§8.8).
- Sequence patterns with order, quantifiers, alternation, and negation (`match_sequence`, §8.6).
- Capture variables and backreferences (`bind`, `attr.eq_ref/.ne_ref`, §8.7).
- Scope-relative predicates (`where_scope`, §8.9).
- Rule composition and cardinality (`compose`, `count`, §8.10).

### 9.1.1 Attribute path mini-language

Attribute matchers use a small, fixed set of suffix operators on an attribute name. There is no general expression language and no nesting beyond the forms below:

| Form                    | Meaning                                                                 |
| ----------------------- | ---------------------------------------------------------------------- |
| `attr: v`               | scalar equality (`attr == v`)                                          |
| `attr.any: [v1, v2]`    | scalar membership (`attr` equals one of the listed values)            |
| `attr.regex: "pat"`     | scalar regex (`attr` matches `pat`)                                    |
| `attr.any.regex: "pat"` | list regex (`attr` is a list; match if **any** element matches `pat`)  |
| `attr.gt/.lt/.ge/.le: n`| numeric comparison of a scalar (numeric) attribute (§8.8)              |
| `attr.eq_ref: var`      | `attr` equals a value captured earlier with `bind` (§8.7)             |
| `attr.ne_ref: var`      | `attr` differs from a value captured earlier with `bind` (§8.7)       |

When several attribute matchers appear under one `attrs:` block, they are ANDed. The `bind`, `eq_ref`, `ne_ref` forms are only meaningful inside a `match_sequence` (§8.6–8.7), where a binding environment flows between steps; the comparison operators work anywhere.

The `unless` clause suppresses a rule when a guard holds. v0 guards:

| Guard                              | Meaning                                                          |
| ---------------------------------- | --------------------------------------------------------------- |
| `path.matches: [glob, ...]`        | the violation's file matches one of the globs                   |
| `waiver.exists: { rule: <id> }`    | a structured waiver (§14) covers this rule for this file        |
| `adr.exists: { topic: <string> }`  | an accepted ADR (§14) records a decision tagged with that topic |

### 9.2 Performance target

The target performance model should be approximately:

```text
O(number_of_relevant_events + number_of_candidate_matches)
```

That bound describes only the **matching** phase. A cold full run is dominated by the work that precedes it: parsing every in-scope file (`O(total_source_bytes)`) and walking every parse tree to emit and index its events (`O(total_events)`). Index construction is therefore `O(total_events)` and amortizes only across many rules or across incremental/diff runs — the per-rule bound above is what stays cheap once the indexes exist, not the cost of building them.

Avoid naive `rules × all_events` scans where possible.

Build indexes:

```text
by_event_kind
by_language
by_file
by_import_source
by_call_name
by_type_name
by_package_name
```

A rule like:

```yaml
match:
  event: Import
  attrs:
    source: "@apollo/client"
```

should only scan `Import` events with source `@apollo/client`.

**Indexing and incremental runs.** The global indexes (`by_import_source`, `by_package_name`, `by_type_name`, …) describe the whole repository, so diff mode cannot rebuild them from the changed files alone. The cache (§10.6) stores per-file events keyed by content hash; an incremental run re-parses only changed files, then rebuilds the indexes by unioning freshly extracted events with the cached events of unchanged files. Whole-repo and manifest-derived rules (e.g. `NO_UNAPPROVED_STATE_LIBRARY`, which reads the dependency manifest) are always evaluated against the complete corpus, even in `--diff` mode.

### 9.3 Regex engine

Use Rust's `regex` crate for attribute-level regular expressions. The crate deliberately omits features such as look-around and backreferences in exchange for strong worst-case performance guarantees. Its documentation states worst-case `O(m * n)` search time, where `m` is proportional to regex size and `n` to the searched text.[^rust-regex]

Per-attribute text matching therefore stays strictly regular and linear. Note that the capture/backreference feature (§8.7) does **not** rely on regex backreferences: it compares captured attribute *strings* across sequence steps through a small binding environment, so the `regex` crate's lack of backreferences is no obstacle. The overall rule language is consequently a regular core (per-attribute regex, sequence NFA) extended with capture-equality and bounded counting — see §9.4 for the exact complexity class.

### 9.4 Expressiveness and complexity

The full proposed rule language is deliberately bounded:

- **Per-attribute predicates** (equality, membership, regex, comparison) are regular and linear (`regex` crate, §9.3).
- **Sequence patterns** (§8.6) compile to a Thompson NFA over the event stream — still regular, linear in the number of events, no backtracking.
- **Capture / backreferences** (§8.7) add a finite environment of bound strings — formally a *register automaton*, which is beyond regular (context-sensitive), but still evaluated in one linear pass with a small, bounded binding set.
- **Composition and cardinality** (§8.10) are a bounded post-pass over violation sets; counting makes the language context-sensitive in the same weak sense.

What the language deliberately does **not** have: unbounded backtracking, a pushdown stack at match time (nesting is resolved upstream into paired scope events), general recursion, or arbitrary mutable state. Every rule terminates in time linear in the event stream (times a small binding/counting factor). This preserves the §18 promise — "cheap enough to run often" — and keeps rules statically reviewable, while reaching the expressiveness of Cobra's declarative pattern and set language.

### 9.5 Relation to Cobra's expressiveness

Cobra's capability is five-tiered. With the constructs in §8.6–8.10, the proposed rule language is **functionally equivalent to Cobra's declarative tiers (1–4)** and deliberately stops short of the fifth:

| Cobra tier                                                                       | Construct here                                                          | Status                |
| -------------------------------------------------------------------------------- | ---------------------------------------------------------------------- | --------------------- |
| 1. Token patterns (sequence, quantifiers, alternation, negation, field predicates) | `match_sequence` (§8.6); structural/comparison predicates (§8.8)       | covered               |
| 2. Name binding + backreferences (`x:@ident … :x`, `(:x != :z)`)                 | `bind`, `attr.eq_ref/.ne_ref` (§8.7)                                   | covered               |
| 3. Scope/bracket navigation (`.jmp`, `contains`, `extend`)                        | `anchor: within`, `where_scope` (§8.6, §8.9) over pre-paired scopes     | covered (declaratively) |
| 4. Named-set algebra + cardinality (`ps C = A & B`, `size(n) > k`)               | `compose`, `count` (§8.10)                                             | covered               |
| 5. Turing-complete inline scripts (`%{ … %}`: loops, arrays, recursion)          | —                                                                      | **deliberate non-goal** |

Two honesty notes on the table. Tier 3 parity is at the level of *declarative scope queries* — enclosing-scope containment, absence, and ordering — not Cobra's arbitrary token-pointer navigation (jump to a matching delimiter, then step an arbitrary number of tokens and inspect); that imperative idiom belongs to Cobra's interactive marking tier and is offered here only through interactive query mode (§16.1), not batch rules. And tiers 2 and 4 are precisely what move the language past strictly-regular into the weak context-sensitive register (§9.4).

Tier 5 is the one Cobra feature this tool intentionally forgoes. Cobra itself reserves it for the checks pattern matching cannot express — lock/unlock pairing across a whole function with aliasing, uninitialized-variable tracking, cyclomatic-complexity metrics. Those are exactly the whole-program, stateful analyses that §4 and §11.4 assign to compilers, type-checkers, and dedicated analyzers. Admitting a general-purpose scripting tier would also break the §8 guarantees (small, reviewable, terminating, cheap). The declarative tiers, by contrast, keep all of those properties — so the proposal matches Cobra's *rule language* without inheriting its *programming language*.

---

## 10. Suggested Rust Implementation

### 10.1 Crate structure

```text
codepolicy/
  Cargo.toml
  crates/
    codepolicy-cli/
    codepolicy-core/
    codepolicy-events/
    codepolicy-rules/
    codepolicy-match/
    codepolicy-report/
    codepolicy-frontends/
      ts_js/
      go/
      python/
      rust/
    codepolicy-vcs/
    codepolicy-cache/
  examples/
    rules/
      frontend-graphql.yaml
      frontend-state.yaml
      config-env.yaml
  fixtures/
    ts_graphql_raw_fetch/
    ts_manual_graphql_type/
    go_env_access/
    python_requests_no_timeout/
    rust_env_access/
```

### 10.2 Core crates

#### `codepolicy-events`

Defines:

- `Event`
- `EventKind`
- `Language`
- `Span`
- typed attribute helpers
- JSON serialization schema

#### `codepolicy-frontends`

Defines the frontend trait:

```rust
use camino::{Utf8Path, Utf8PathBuf};

/// A source file handed to a frontend: its path plus a borrow of its text.
pub struct SourceFile<'a> {
    pub path: Utf8PathBuf,
    pub text: &'a str,
}

/// A frontend's output: the universal **token stream** (the primary model,
/// produced by every source frontend) plus an optional **canonical event**
/// overlay (the normalized, cross-language layer a frontend adds if it can).
pub struct Extracted {
    pub tokens: Option<TokenStream>, // compact language-local token layer (see below)
    pub events: Vec<Event>,          // optional normalized events (Import, Call, EnvAccess, …)
}

pub trait LanguageFrontend {
    fn name(&self) -> &str;
    fn supports_file(&self, path: &Utf8Path) -> bool;
    /// Every frontend yields the token stream (by lexing or parsing — the
    /// mechanism is the frontend's choice); the canonical `events` overlay is
    /// optional. `want_tokens` lets the pipeline skip materializing the token
    /// stream when no loaded rule references `Token`.
    fn extract(&self, file: &SourceFile<'_>, want_tokens: bool) -> anyhow::Result<Extracted>;
}
```

The token stream is the universal substrate; canonical events are an optional structured overlay. A frontend may be parser-based (tree-sitter, yielding both) or lexer-based (yielding tokens only) — the trait does not prescribe how. The TS/JS frontend uses Tree-sitter for both from a single parse.

**Compact token representation.** Because there is one token per parse-tree node, the token layer dominates volume on a large repo. Materializing it as fat `Event`s (each carrying an `Arc<Utf8PathBuf>`, a `BTreeMap<String, Value>` of attributes, and owned strings) is what does not scale — not the walk itself, which is already `O(N)`. Following Cobra's linked-list-of-tokens design, the token layer is therefore a flat, columnar `TokenStream`:

```rust
pub type Sym = u32;            // interned string id
pub const NO_JMP: u32 = u32::MAX;

#[derive(Clone, Copy)]        // 1 token = a few machine words, no per-token heap
pub struct Token {
    pub kind: Sym,            // interned node_kind, e.g. "switch_statement"
    pub text: Sym,            // interned source text
    pub func: Sym,            // interned enclosing-function name (Cobra's .fct)
    pub named: bool,          // tree-sitter named node vs. anonymous symbol literal
    pub start_byte: u32, pub end_byte: u32,
    pub start_line: u32, pub start_col: u32,
    pub end_line: u32, pub end_col: u32,
    pub curly: u16, pub round: u16, pub bracket: u16, // nesting depths (.curly/.round)
    pub jmp: u32,            // index of the matching delimiter, or NO_JMP
}

pub struct TokenStream {
    pub file: Arc<Utf8PathBuf>,
    pub language: Language,
    pub interner: Interner,  // Sym -> &str
    pub tokens: Vec<Token>,  // implicit prev/next via index ±1
}
```

Each token is `Copy` and field-packed; all strings are interned once per file, so repeated `node_kind`s and identifiers cost 4 bytes each. Prev/next are implicit (index ±1), and `jmp` gives Cobra's matching-delimiter link (paired in a single post-pass stack walk over `()`/`{}`/`[]`). A `Token` rule matches directly against these fields — `node_kind`, `text`, `function`, `named`, `curly_depth`/`round_depth`/`bracket_depth`, `range_lines`, `text_len` — with no per-token allocation. Canonical events stay fat because there are few of them; tokens go compact because there are many. The stream is still pay-for-what-you-use: it is built only when a loaded rule references `Token` (`want_tokens`). Token rules are currently single-event only (`match Token[...]`); `match_sequence`/`where_scope` over the token stream are rejected at compile time.

#### `codepolicy-rules`

Defines:

- rule schema
- YAML/TOML parsing
- rule validation
- compiled rule representation

#### `codepolicy-match`

Defines:

- event indexes
- matcher engine
- sequence/region matcher later
- violation generation

#### `codepolicy-report`

Defines:

- human terminal output
- JSON output
- SARIF output later
- LLM-friendly output

#### `codepolicy-vcs`

Defines:

- Git diff support
- changed file detection
- package dependency diff support

#### `codepolicy-cache`

Defines:

- file hash cache
- event cache
- incremental re-checking

### 10.3 CLI shape

```bash
codepolicy check
codepolicy check --diff main...HEAD
codepolicy events path/to/file.tsx
codepolicy query 'Import[source="@apollo/client"]'
codepolicy explain-rule NO_DIRECT_GRAPHQL_CLIENT
codepolicy init
```

### 10.4 Output format

Human output:

```text
ERROR NO_DIRECT_GRAPHQL_CLIENT
apps/admin/src/features/members/Member.tsx:1:10

Feature code must use the approved GraphQL access layer.

Matched:
  Import source="@apollo/client" symbols=["useQuery"]

Remediation:
  Use @app/graphql generated hooks instead of importing Apollo directly.
```

JSON output:

```json
{
  "violations": [
    {
      "rule_id": "NO_DIRECT_GRAPHQL_CLIENT",
      "severity": "error",
      "file": "apps/admin/src/features/members/Member.tsx",
      "span": {
        "start_line": 1,
        "start_col": 10,
        "end_line": 1,
        "end_col": 18
      },
      "message": "Use @app/graphql generated hooks instead of importing Apollo directly.",
      "matched_event": {
        "kind": "Import",
        "attrs": {
          "source": "@apollo/client",
          "symbols": ["useQuery"]
        }
      }
    }
  ]
}
```

A violation's `span` points at the **matched attribute/token** (here the `useQuery` symbol starting at column 10), which is narrower than the enclosing event's `span` in §7.3 (which covers the whole `import` statement from column 1). Both follow the 1-based, end-exclusive convention of §7.1.

### 10.5 Recommended Rust dependencies

Initial candidates:

```toml
[dependencies]
anyhow = "1"
thiserror = "1"
clap = { version = "4", features = ["derive"] }
serde = { version = "1", features = ["derive"] }
serde_json = "1"
serde_yaml_ng = "0.9"  # maintained fork; upstream serde_yaml was deprecated/archived in 2024
camino = { version = "1", features = ["serde1"] }  # serde1 needed for Utf8PathBuf (de)serialization
ignore = "0.4"
globset = "0.4"
regex = "1"
once_cell = "1"  # optional: on MSRV >= 1.80, prefer std::sync::{OnceLock, LazyLock}
rayon = "1"
tracing = "0.1"
tracing-subscriber = "0.3"
tree-sitter = "0.26"
```

Language grammars may include:

```toml
tree-sitter-javascript = "..."
tree-sitter-typescript = "..."
tree-sitter-go = "..."
tree-sitter-python = "..."
tree-sitter-rust = "..."
```

Use exact versions when creating the actual `Cargo.toml`. Tree-sitter grammar crates are tightly coupled to the core library's ABI version and release on independent schedules, so verify that each grammar targets an ABI compatible with the chosen `tree-sitter` core — an ABI mismatch causes runtime parser failures. Pin exact, mutually compatible versions.

Tree-sitter is a parser generator and incremental parsing library that builds concrete syntax trees and updates them efficiently as code changes.[^tree-sitter] It therefore produces full parse trees, not a flat token list: each frontend walks the tree and **projects it down** to flat canonical events. This adds an explicit translation layer that Cobra — which works directly on a lexical token stream — does not have. The Cobra inspiration is the *philosophy* (cheap pattern rules over a flat stream, with concrete file/line evidence), not the lexical substrate itself; rule matching stays event-focused rather than tree-traversal-based.

---

## 11. V0 Scope

### 11.1 Languages

Start with TypeScript/JavaScript because the motivating policies are frontend-oriented.

Recommended v0 language order:

1. TypeScript/JavaScript/TSX
2. Python
3. Go
4. Rust

Because the tool itself is written in Rust, the Rust frontend (item 4) is added together with Python and Go in §17 Phase 5; it is especially valuable for dogfooding the tool on its own codebase. The cross-language `NO_DIRECT_ENV_ACCESS` rule is the first proof that one rule spans languages.

### 11.2 V0 events

Implement only:

```text
File
PackageAdded
Import
Call
TypeDecl
FunctionDecl
EnvAccess
StringLiteral
Comment
```

`ScopeStart`/`ScopeEnd` and the advanced constructs that depend on them — sequence patterns (§8.6), capture/backreferences (§8.7), scope-relative predicates (§8.9), and composition/cardinality (§8.10) — are deferred to post-v0. v0 implements the single-event subset (§8.2–8.5); the structural-field and comparison predicates (§8.8) arrive with whichever frontend first computes nesting/range. The advanced constructs are what bring the tool to declarative parity with Cobra (§9.5); they are part of the proposed design, not the first cut.

### 11.3 V0 rules

Ship with example rules:

1. `NO_DIRECT_GRAPHQL_CLIENT`
2. `NO_RAW_GRAPHQL_FETCH`
3. `NO_MANUAL_GRAPHQL_OPERATION_TYPES`
4. `NO_UNAPPROVED_STATE_LIBRARY`
5. `NO_DIRECT_ZUSTAND_OUTSIDE_STATE_PACKAGE`
6. `NO_DIRECT_ENV_ACCESS`
7. `NO_PROVIDER_SDK_OUTSIDE_INFRA`
8. `NO_TODO_WITHOUT_ISSUE`

> **Producer note.** `NO_UNAPPROVED_STATE_LIBRARY` (and any other `PackageAdded` rule) depends on a manifest frontend that emits `PackageAdded` by diffing `package.json` / `go.mod` / `requirements.txt`. That producer is part of the dependency-diff support in §17 Phase 6, so either schedule a minimal manifest reader in Phases 1–3 or defer this rule until Phase 6. `PackageAdded` is past-tense by design: it is meaningful chiefly in `--diff` mode.

### 11.4 V0 non-goals

Do not implement in v0:

- Full type inference.
- Whole-program dataflow.
- Alias analysis.
- Macro expansion.
- Complex interprocedural analysis.
- Perfect state-management classification.
- Perfect authorization correctness checking.
- Turing-complete rule scripting (Cobra's inline-program tier, §9.5). Stateful whole-function/whole-program analyses belong to compilers, type-checkers, and dedicated analyzers — not to the rule language, which stays declarative and terminating (§9.4).

Those are review/test/type-system concerns, not token-rule concerns.

---

## 12. Example Policy Pack

### 12.1 Frontend GraphQL

```yaml
rules:
  - id: NO_DIRECT_GRAPHQL_CLIENT
    severity: error
    description: Feature code must use the approved GraphQL access layer.
    applies_to:
      languages: [typescript, javascript]
      paths:
        include:
          - "apps/*/src/**/*.{ts,tsx,js,jsx}"
          - "packages/**/*.{ts,tsx,js,jsx}"
        exclude: ["packages/graphql/**"]
    match:
      event: Import
      attrs:
        source: "@apollo/client"
        symbols.any: ["useQuery", "useMutation", "gql", "ApolloClient"]
    message: "Use @app/graphql generated hooks instead of importing Apollo directly."

  - id: NO_RAW_GRAPHQL_FETCH
    severity: error
    description: GraphQL calls must go through generated operations and approved hooks.
    applies_to:
      languages: [typescript, javascript]
      paths:
        include: ["apps/*/src/**/*.{ts,tsx,js,jsx}"]
    match:
      event: Call
      attrs:
        name: "fetch"
        string_args.any.regex: ".*graphql.*"
    message: "Do not call the GraphQL endpoint directly. Use @app/graphql."

  - id: NO_MANUAL_GRAPHQL_OPERATION_TYPES
    severity: error
    description: GraphQL operation types must be generated.
    applies_to:
      languages: [typescript]
      paths:
        include: ["apps/*/src/**/*.{ts,tsx}"]
        exclude: ["**/generated/**", "**/*.generated.ts"]
    match:
      event: TypeDecl
      attrs:
        name.regex: ".*(Query|Mutation|Subscription)(Variables)?$"
    message: "Do not hand-write GraphQL operation types. Use generated types."
```

> `NO_MANUAL_GRAPHQL_OPERATION_TYPES` is a heuristic name-suffix check — see the false-positive caveat in §8.3.

### 12.2 Frontend state

```yaml
rules:
  - id: NO_UNAPPROVED_STATE_LIBRARY
    severity: error
    description: Frontend state management libraries must be explicitly approved.
    match:
      event: PackageAdded
      attrs:
        name.any:
          - redux
          - "@reduxjs/toolkit"
          - recoil
          - jotai
          - mobx
          - xstate
    unless:
      adr.exists:
        topic: "state management exception"
    message: "Use Zustand/@app/state unless an ADR justifies another state library."

  - id: NO_FORBIDDEN_STATE_IMPORT
    severity: error
    description: Feature code must not import unapproved state libraries.
    applies_to:
      languages: [typescript, javascript]
      paths:
        include: ["apps/*/src/**/*.{ts,tsx,js,jsx}"]
    match:
      event: Import
      attrs:
        source.any:
          - redux
          - "@reduxjs/toolkit"
          - recoil
          - jotai
          - mobx
          - xstate
    unless:
      adr.exists:
        topic: "state management exception"
    message: "Use Zustand/@app/state unless an ADR justifies another state library."

  - id: NO_DIRECT_ZUSTAND_OUTSIDE_STATE_PACKAGE
    severity: warning
    description: Feature code should use the project state wrapper, not Zustand directly.
    applies_to:
      languages: [typescript, javascript]
      paths:
        include: ["apps/*/src/**/*.{ts,tsx,js,jsx}"]
        exclude: ["packages/state/**"]
    match:
      event: Import
      attrs:
        source: "zustand"
    message: "Import state helpers from @app/state instead of importing Zustand directly."
```

### 12.3 Cross-language config policy

```yaml
rules:
  - id: NO_DIRECT_ENV_ACCESS
    severity: error
    description: Runtime environment variables must be read through the project config layer.
    applies_to:
      languages: [typescript, javascript, go, python, rust]
      paths:
        include: ["**/*"]
        exclude:
          - "**/config/**"
          - "**/settings/**"
    match:
      event: EnvAccess
    message: "Read environment variables through the typed config/settings layer."
```

---

## 13. LLM Coding Agent Workflow

The tool should be designed to work with a coding agent.

Suggested workflow:

```text
1. Human asks agent to implement feature.
2. Agent modifies repo.
3. Harness runs `codepolicy check --diff main...HEAD`.
4. Tool emits violations with exact file/line/rule evidence.
5. Agent receives violations as compiler-like errors.
6. Agent fixes code.
7. Harness re-runs until clean or explicit waiver/ADR is added.
```

The LLM should not be asked merely to remember rules. It should receive hard feedback:

```text
ERROR NO_DIRECT_GRAPHQL_CLIENT
apps/admin/src/features/members/Member.tsx:1:10

Matched Import source="@apollo/client" symbol="useQuery".
Use @app/graphql generated hooks instead.
```

This converts architectural and style guidance into a tight feedback loop.

---

## 14. ADRs and Waivers

Some rules need escape hatches.

Use structured ADR/waiver metadata, not arbitrary comments.

Example waiver:

```yaml
id: WAIVER-0007
rule: NO_DIRECT_ZUSTAND_OUTSIDE_STATE_PACKAGE
file: apps/admin/src/features/debug/debugStore.ts
reason: Temporary migration path while @app/state wrapper is being introduced.
expires: 2026-09-01
approved_by: lead-engineer
```

Policy:

- Waivers must be structured.
- Waivers should expire.
- Waivers should be reported in CI output.
- LLM agents may propose waivers but should not silently create them as a default solution.

The two escape hatches differ in scope and correspond to the two `unless` guards in §9.1.1:

- **Waiver** (`waiver.exists: { rule }`) — a narrow, file-scoped, expiring exception for a specific rule, using the schema above (keyed on `rule` + `file`).
- **ADR** (`adr.exists: { topic }`) — a repository-wide architectural decision that justifies a class of usage, matched by `topic`.

Example ADR:

```yaml
id: ADR-0042
topic: "state management exception"
status: accepted
decision: Allow XState in the workflow-engine package; it models statecharts the Zustand wrapper cannot express.
date: 2026-04-01
approved_by: architecture-group
```

ADRs live under a project-configured directory (e.g. `docs/adr/`). `adr.exists` matches when an ADR with `status: accepted` and the given `topic` is present; `topic` matching is exact.

---

## 15. Testing Strategy

The rule system itself must be tested.

### 15.1 Fixture tests

Create fixtures:

```text
fixtures/
  ts_graphql_ok_generated_hook/
  ts_graphql_bad_raw_fetch/
  ts_graphql_bad_manual_type/
  ts_state_bad_jotai_import/
  go_bad_env_access/
  python_bad_env_access/
  rust_bad_env_access/
```

Each fixture should include:

```text
input source files
expected events JSON
expected violations JSON
```

### 15.2 Snapshot tests

Use snapshot testing for:

- Event streams.
- Rule violations.
- CLI output.

### 15.3 Mutation-style tests

For important policies, deliberately mutate known-good code into bad code and ensure the tool catches it.

Examples:

- Replace `useGraphqlQuery` with `fetch('/graphql')`.
- Add `jotai` to package dependencies.
- Add `interface GetMemberQuery` outside generated folders.
- Replace config access with `process.env.API_KEY`.

---

## 16. Future Extensions

### 16.1 Interactive query mode

Inspired by Cobra, eventually support interactive exploration:

```bash
codepolicy query 'Import[source="@apollo/client"]'
codepolicy query 'Call[name="fetch" string_args.any=/graphql/]'
```

The interactive query language is a surface syntax over the same predicates as the rule DSL (§9.1.1):

| Query syntax         | Rule DSL equivalent       |
| -------------------- | ------------------------- |
| `attr="v"`           | `attr: "v"`               |
| `attr=/regex/`       | `attr.regex: "regex"`     |
| `attr.any=["a","b"]` | `attr.any: ["a", "b"]`    |
| `attr.any=/regex/`   | `attr.any.regex: "regex"` |

### 16.2 SARIF output

Emit SARIF so results appear in GitHub code scanning.

### 16.3 OPA/Rego aggregation

This tool should produce violations and check results. OPA/Rego can then aggregate broader merge policy. The block below is **illustrative policy intent, not literal Rego syntax** (real Rego uses rule heads with bodies, e.g. `deny contains msg if { ... }`):

```text
deny if codepolicy has errors
deny if typecheck failed
deny if tests failed
require review if warning touches auth, billing, tenant isolation, or migrations
```

### 16.4 Codegen freshness checks

Add specialized checks:

```text
If GraphQL operation files changed, generated output must be fresh.
If OpenAPI schema changed, generated client must be fresh.
If Prisma/Drizzle schema changed, generated artifacts must be fresh.
```

A simple implementation can run the generator and assert `git diff --exit-code`.

### 16.5 MCP server

Expose the event index and query engine through MCP so an LLM coding agent can ask:

```text
Which files import @apollo/client directly?
Where is process.env accessed outside config?
Which packages use direct database access?
```

This would make the tool not just a CI gate, but an interactive codebase memory system for agents.

---

## 17. Implementation Plan for Coding Agent

### Phase 1: Rust CLI skeleton

Build:

```text
codepolicy check
codepolicy events <file>
codepolicy --rules <rules.yaml>
```

Implement:

- CLI using `clap`.
- File discovery using `ignore`.
- Basic event schema.
- JSON output.
- Rule loading from YAML.

### Phase 2: TypeScript/JavaScript frontend

Implement event extraction for:

- imports
- calls
- type declarations
- env access
- comments
- string literals

Add rules:

- direct Apollo import
- raw GraphQL fetch
- manual GraphQL operation types
- direct Zustand import outside state package

### Phase 3: Pattern matcher

Implement:

- path include/exclude
- event kind matching
- attr equality
- attr list contains
- attr regex
- simple `unless` for path and ADR/waiver presence

### Phase 4: Test fixtures

Create good/bad fixtures and snapshot expected event/violation output.

### Phase 5: Python/Go/Rust frontends

Add shallow extractors for:

- imports/use declarations
- calls
- env access
- function/type declarations
- comments

Start with cross-language `NO_DIRECT_ENV_ACCESS` as the first proof of language-neutral rules.

### Phase 6: Cache and diff mode

Implement:

- file hashing
- changed-file-only mode
- Git diff support
- package dependency diff support

### Phase 7: LLM-friendly reports

Add output mode:

```bash
codepolicy check --format agent
```

The report should include:

- rule ID
- severity
- exact file/line
- matched event
- why it failed
- canonical remediation

---

## 18. Guiding Philosophy

The tool should remain humble.

It should not try to prove that the architecture is beautiful. It should prove that important project boundaries are not casually violated.

It should not infer all design intent. It should force design intent through visible, checkable seams.

It should not replace human review. It should eliminate avoidable drift before human review.

The ideal rule is:

```text
small enough to understand
precise enough to enforce
cheap enough to run often
specific enough to correct an LLM agent
```

This project is valuable because it turns soft engineering taste into hard feedback loops.

---

## 19. References

[^cobra-github]: Cobra GitHub repository, `nimble-code/Cobra`, describing Cobra as a fast analyzer for querying millions of lines of code, originally developed at NASA/JPL and publicly released in 2016: <https://github.com/nimble-code/Cobra/>

[^space-ros-cobra]: Space ROS documentation on Cobra, describing lexical analysis into a language-level token stream followed by rule-set pattern searches: <https://space-ros.github.io/docs/rolling/Related-Projects/Cobra.html>

[^cobra-manual]: Cobra Reference Manual, describing Cobra's lexical analyzer and linked-list token data structure with annotations and matching delimiter links: <https://codescrub.com/cobra/manual.html>

[^cobra-patterns]: Cobra Pattern Searches documentation, explaining token-level pattern matching and why it is more precise than raw grep: <https://spinroot.com/cobra/pattern_searches.html>

[^tree-sitter]: Tree-sitter official documentation, describing Tree-sitter as a parser generator and incremental parsing library: <https://tree-sitter.github.io/>

[^rust-regex]: Rust `regex` crate documentation, describing the omission of look-around/backreferences and worst-case `O(m * n)` search time: <https://docs.rs/regex/latest/regex/>
