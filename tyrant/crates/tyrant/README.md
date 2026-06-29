# tyrant

**tyrant** is a rule engine for source code. You write rules — declarative
patterns over a file's **lexemes** — and it scans a repository and reports every
match as `file:line` evidence with a message. It has no built-in notion of
correct code; it enforces exactly the rules you write.

It follows NASA/JPL's Cobra model: a flat stream of lexical tokens, matched by
patterns, never raw text — so a rule for the identifier `x` matches the lexeme
`x`, not the `x` inside `"prefix"` or a comment.

Exit codes — `0` clean, `1` an error-level violation, `2` a malformed rules file
— so it drops into a pre-commit hook, a CI gate, or an LLM agent loop. The
`agent` output format makes each violation machine-readable (`rule`, `file:line`,
fix message) so a model can correct its own output before a human sees it.

## Install

```bash
cargo install tyrant
```

This installs the `tyrant` binary.

## Quickstart

```bash
tyrant init                            # write a starter rule pack
tyrant check                           # check the current repo
tyrant check src/ --format agent       # check a subtree, agent-readable output
tyrant check --rules my.rules --format json
tyrant tokens path/to/File             # dump a file's lexeme stream
tyrant explain-rule no_explicit_any    # show how a rule compiles
```

`check` discovers a `tyrant.rules` or `tyrant.yaml` at the repo root (or takes
`--rules <file>`) and lexes supported files in parallel. Output formats are
**human**, **json** (`{ "violations": [...], "summary": {...} }`), and **agent**
(terse `SEVERITY rule_id at file:line:col` with `matched:` / `why:` / `fix:`).

## A rule

```
rule no_explicit_any (warning) {
  lang typescript, javascript
  in  "src/**"
  not in "**/*.test.ts"
  match "any"
  message "Avoid the `any` type; use a precise type or `unknown`."
}
```

A rule matches token patterns (literal lexemes, `@class` token classes,
`/regex/`), ordered `sequence`s with balanced delimiters and captures, scope
predicates, and `compose`/`count` derivations. Bundled language frontend:
TypeScript/JavaScript (via tree-sitter).

## Documentation

Full rule-language reference, architecture, and design rationale:
<https://github.com/umran/tyrant>.

## License

Licensed under either of [MIT](https://github.com/umran/tyrant/blob/main/LICENSE-MIT)
or [Apache-2.0](https://github.com/umran/tyrant/blob/main/LICENSE-APACHE) at your
option.
