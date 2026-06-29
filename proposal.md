# tyrant — design notes

`tyrant` is a fast, multi-language static policy tool inspired by NASA/JPL's
Cobra. You write rules — declarative patterns over a file's **lexemes** — and it
reports every match with `file:line` evidence. It has no built-in opinion about
good code; it enforces the rules you write.

This document is the design rationale. The [README](README.md) is the
authoritative user reference for the grammar and CLI.

> Earlier drafts of this project carried a second "canonical event" layer (a
> normalized, parse-derived construct model: `Import`, `Call`, `TypeDecl`, …).
> That layer was removed: the tool is now a single, token-only lexeme engine, in
> the spirit of Cobra, which lexes rather than parses.

## 1. The idea

Many useful code checks do not need full parsing, type inference, alias analysis,
or data-flow analysis. They can be expressed as patterns over a **flat stream of
lexical tokens** plus a small amount of per-token annotation.

Cobra's value over `grep` is exactly this: `grep x` matches `x` inside the
identifier `prefix`, inside the string `"x"`, and inside comments; a token
pattern for `x` matches only the identifier *lexeme* `x`, because the lexer has
already classified `prefix`, `"x"`, and the comment as their own atomic tokens.

So tyrant works on the lexer's output — a token stream — and never on raw
text.

## 2. The token model

A frontend lexes a file into a flat `Vec<Token>`. Each `Token` is `Copy` and
field-packed; strings (node kind, class, text, enclosing function) are interned
to `u32` ids per file, so the stream is one contiguous allocation with no
per-token heap. Previous/next are implicit (index ±1), and each delimiter links
to its match via a `jmp` index (Cobra's matching-delimiter links), which is how
scopes are defined.

```rust
pub struct Token {
    kind: Sym,            // interned node_kind, e.g. "identifier", "switch", "=="
    text: Sym,            // interned source text (capped)
    func: Sym,            // interned enclosing-function name
    class: Sym,           // normalized lexeme class (ident/str/num/comment/symbol/…)
    named: bool,          // named lexeme vs. anonymous symbol
    start_byte: u32, end_byte: u32,
    start_line: u32, start_col: u32, end_line: u32, end_col: u32,
    curly: u16, round: u16, bracket: u16,   // nesting depths
    jmp: u32,             // index of the matching delimiter, or NO_JMP
}
```

The stream is a **lexeme** stream, not a parse-node stream: only leaf lexemes are
tokens. A string or template literal is one atomic token (matching never reaches
inside it). Comments are tokens (class `comment`). Composite constructs (a block,
a call) are *not* tokens — you match a block by its `{`, a call by the callee
lexeme followed by `(`.

**Normalized class.** `node_kind` is the raw grammar name and is language-
specific; `class` is a small, normalized, language-neutral category the frontend
assigns (`ident`, `prop`, `str`, `num`, `bool`, `regex`, `comment`, `symbol`).
`@ident` checks `class`, so a rule stays language-agnostic across frontends.

## 3. The matching engine

Rules compile to matchers over the token stream:

- **Single match** — a literal lexeme (`debugger`, `"=="`), `@class`, `/regex/`,
  or `^/regex/` (negation) — one lexeme whose text/class satisfy the pattern.
- **Sequence** — an ordered run of token patterns with quantifiers (`? * +`),
  negation, alternation, the `any`/`.` wildcard, and captures/backreferences. The
  matcher tries every start position and is end-anchored over its region, so a
  trailing `not X *` means "no `X` in the rest of the region." Explicit `( )` /
  `{ }` / `[ ]` steps balance against their `jmp` partner, so a wildcard cannot
  escape the pair. `in scope` restricts the region to a `{}` block.
- **Scope predicates** — `where scope contains/not contains/followed by` over the
  matched lexeme's enclosing `{}` block.
- **Composition / counting** — `compose` (set algebra) and `count` (cardinality)
  run as a post-pass over other rules' violations, keyed by file and/or function.

The surface is Cobra's token-pattern syntax directly: a bare word is a literal
lexeme (`for`), `/^pumba/` a text regex, `@ident` a class, `@ident & /^pumba/` a
conjunction, `.`/`any` the wildcard, `* +` step repetition, `[ a b ]` / `( a | b )`
a set/choice, `^X` negation, and `x:@ident … :x` a binding and backreference. A
raw `Token[field op v]` form remains for lexeme fields with no surface syntax
(`curly_depth`, `text_len`).

## 4. Frontends

A language is one trait:

```rust
pub trait Frontend: Sync {
    fn name(&self) -> &str;
    fn supports_file(&self, path: &Utf8Path) -> bool;
    fn lex(&self, file: &SourceFile<'_>) -> anyhow::Result<TokenStream>;
}
```

A frontend may be a hand-written lexer or, as with the bundled TypeScript/
JavaScript frontend, derive the leaf-token stream from a tree-sitter parse. The
matching engine, the grammar, and the rules are entirely language-agnostic; a
rule applies to any language whose frontend assigns the same `class` names.

## 5. Escape hatches

- **Inline `unless`** on a rule: `unless path "glob"`, `unless adr "topic"`,
  `unless waiver`.
- **Waivers** — file-scoped, per-rule exceptions in `.tyrant/waivers/`.
- **ADRs** — repo-wide accepted decisions in `docs/adr/`, matched by topic.

## 6. Performance

The token stream is cheap: a flat interned array, scanned linearly, with
single-token matching being regular. Producing it needs only a lexer. The design
target is running over millions of lines, so the representation avoids per-token
allocation and the pipeline lexes files in parallel.

## 7. Non-goals (honesty notes)

- **No type inference.** A `: kind`-style annotation, were it offered, would be a
  *syntactic* form (how an expression is written), never an inferred static type.
- **No data-flow / alias / control-flow analysis.** Correlation is limited to
  same-value backreferences over the token sequence, not def-use or aliasing.
- **No whole-program / cross-file reasoning** beyond `compose`/`count`
  aggregation over per-file violations.

Those belong to type-checkers and dedicated analyzers, not to a token-pattern
engine.
