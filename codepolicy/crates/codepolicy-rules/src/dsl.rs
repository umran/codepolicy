//! A concise, Cobra-flavored textual rule grammar that parses into the same
//! [`RawRule`](super::RawRule) structures the YAML loader produces, then reuses
//! the compiler. Rules match over the lexeme (token) stream.
//!
//! ```text
//! rule NO_DEBUGGER (error) {
//!   lang typescript, javascript
//!   match Token[node_kind = "debugger"]
//!   message "Remove debugger statements."
//! }
//! ```
//!
//! Token matchers:
//!   `Token[ field <op> v, ... ]`   explicit field predicates
//!   `@class`                       a token class (`@ident`, `@str`, `@num`, …)
//!   `/regex/`                      a token whose text matches the regex
//!   `@ident & /re/`                conjunction on one token
//!   `any`                          any one token (sequence wildcard)
//!
//! Predicate operators (inside `Token[ ... ]`):
//!   `field = "v"` / `field = 5`   equality            (`field`)
//!   `field ~ /re/`                regex               (`field.regex`)
//!   `field !~ /re/`               negated regex        (`field.not.regex`)
//!   `field in ["a","b"]`         membership           (`field.any`)
//!   `field > 5` / `< <= >=`      numeric comparison    (`field.gt` …)
//!   `field == $v`                equals a binding      (`field.eq_ref`)
//!   `field != $v`                differs from binding  (`field.ne_ref`)
//!
//! Bodies: `match <pat> [where scope <clause>]*`, `sequence [in scope] { <step>+ }`,
//! `compose <op> of A, B [by file, function]`, `count RULE per file > N`.

use super::{
    AdrGuard, AppliesTo, CompiledRule, PathFilter, RawAnchor, RawCompose, RawCount, RawMatch,
    RawRule, RawSequence, RawStep, RawUnless, RawWhereScope, RuleFile, Severity, WaiverGuard,
};
use codepolicy_token::Language;
use serde_yaml_ng::Value;
use std::collections::BTreeMap;

/// Token-pattern predicates, keyed by the compiler's suffix convention.
type Attrs = BTreeMap<String, Value>;
/// Captures introduced by a step: variable name -> source field.
type Binds = BTreeMap<String, String>;

#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct DslError(pub String);

type R<T> = Result<T, DslError>;

/// Parse and compile rules written in the textual grammar.
pub fn load(text: &str) -> R<(Vec<CompiledRule>, RuleFile)> {
    let file = parse(text)?;
    let rules = file
        .rules
        .iter()
        .map(super::compile_rule)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| DslError(format!("{e}")))?;
    Ok((rules, file))
}

/// Parse the textual grammar into a [`RuleFile`] (no compilation).
pub fn parse(text: &str) -> R<RuleFile> {
    let toks = lex(text)?;
    let mut p = Parser { toks, pos: 0 };
    p.parse_file()
}

// ---------------------------------------------------------------------------
// Lexer
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
enum Tok {
    Ident(String),
    Str(String),
    Regex(String),
    /// A `@class` token-class matcher: `@ident`, `@str`, `@num`, …
    TypeClass(String),
    Num(String),
    LBrace,
    RBrace,
    LBrack,
    RBrack,
    LParen,
    RParen,
    Comma,
    Eq,
    EqEq,
    Ne,
    Ge,
    Le,
    Gt,
    Lt,
    Tilde,
    NotTilde,
    Star,
    Plus,
    Question,
    Pipe,
    Amp,
    Dollar,
}

fn lex(src: &str) -> R<Vec<(Tok, usize)>> {
    let b: Vec<char> = src.chars().collect();
    let mut i = 0;
    let mut line = 1usize;
    let mut out = Vec::new();
    while i < b.len() {
        let c = b[i];
        if c == '\n' {
            line += 1;
            i += 1;
            continue;
        }
        if c.is_whitespace() {
            i += 1;
            continue;
        }
        if c == '#' {
            while i < b.len() && b[i] != '\n' {
                i += 1;
            }
            continue;
        }
        let two = |a: char| i + 1 < b.len() && b[i + 1] == a;
        let push = |out: &mut Vec<(Tok, usize)>, t: Tok| out.push((t, line));
        match c {
            '{' => {
                push(&mut out, Tok::LBrace);
                i += 1;
            }
            '}' => {
                push(&mut out, Tok::RBrace);
                i += 1;
            }
            '[' => {
                push(&mut out, Tok::LBrack);
                i += 1;
            }
            ']' => {
                push(&mut out, Tok::RBrack);
                i += 1;
            }
            '(' => {
                push(&mut out, Tok::LParen);
                i += 1;
            }
            ')' => {
                push(&mut out, Tok::RParen);
                i += 1;
            }
            ',' => {
                push(&mut out, Tok::Comma);
                i += 1;
            }
            '|' => {
                push(&mut out, Tok::Pipe);
                i += 1;
            }
            '&' => {
                push(&mut out, Tok::Amp);
                i += 1;
            }
            '@' => {
                // `@class` token-class matcher: `@` followed by an identifier.
                let start = i + 1;
                let mut j = start;
                while j < b.len() && is_ident_char(b[j]) {
                    j += 1;
                }
                if j == start {
                    return Err(DslError(format!("line {line}: expected a class name after `@`")));
                }
                push(&mut out, Tok::TypeClass(b[start..j].iter().collect()));
                i = j;
            }
            '$' => {
                push(&mut out, Tok::Dollar);
                i += 1;
            }
            '*' => {
                push(&mut out, Tok::Star);
                i += 1;
            }
            '+' => {
                push(&mut out, Tok::Plus);
                i += 1;
            }
            '?' => {
                push(&mut out, Tok::Question);
                i += 1;
            }
            '~' => {
                push(&mut out, Tok::Tilde);
                i += 1;
            }
            '=' if two('=') => {
                push(&mut out, Tok::EqEq);
                i += 2;
            }
            '=' => {
                push(&mut out, Tok::Eq);
                i += 1;
            }
            '!' if two('=') => {
                push(&mut out, Tok::Ne);
                i += 2;
            }
            '!' if two('~') => {
                push(&mut out, Tok::NotTilde);
                i += 2;
            }
            '>' if two('=') => {
                push(&mut out, Tok::Ge);
                i += 2;
            }
            '>' => {
                push(&mut out, Tok::Gt);
                i += 1;
            }
            '<' if two('=') => {
                push(&mut out, Tok::Le);
                i += 2;
            }
            '<' => {
                push(&mut out, Tok::Lt);
                i += 1;
            }
            '"' => {
                let (s, ni) = read_string(&b, i + 1, line)?;
                push(&mut out, Tok::Str(s));
                i = ni;
            }
            '/' => {
                let (s, ni) = read_regex(&b, i + 1, line)?;
                push(&mut out, Tok::Regex(s));
                i = ni;
            }
            c if c.is_ascii_digit() => {
                let start = i;
                while i < b.len() && (b[i].is_ascii_digit() || b[i] == '.') {
                    i += 1;
                }
                push(&mut out, Tok::Num(b[start..i].iter().collect()));
            }
            c if is_ident_start(c) => {
                let start = i;
                while i < b.len() && is_ident_char(b[i]) {
                    i += 1;
                }
                push(&mut out, Tok::Ident(b[start..i].iter().collect()));
            }
            other => {
                return Err(DslError(format!("line {line}: unexpected character `{other}`")));
            }
        }
    }
    Ok(out)
}

fn is_ident_start(c: char) -> bool {
    c.is_ascii_alphabetic() || c == '_'
}
fn is_ident_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_'
}

fn read_string(b: &[char], mut i: usize, line: usize) -> R<(String, usize)> {
    let mut s = String::new();
    while i < b.len() {
        match b[i] {
            '"' => return Ok((s, i + 1)),
            '\\' if i + 1 < b.len() => {
                s.push(b[i + 1]);
                i += 2;
            }
            c => {
                s.push(c);
                i += 1;
            }
        }
    }
    Err(DslError(format!("line {line}: unterminated string")))
}

fn read_regex(b: &[char], mut i: usize, line: usize) -> R<(String, usize)> {
    let mut s = String::new();
    while i < b.len() {
        match b[i] {
            '/' => return Ok((s, i + 1)),
            '\\' if i + 1 < b.len() && b[i + 1] == '/' => {
                s.push('/');
                i += 2;
            }
            '\\' if i + 1 < b.len() => {
                s.push('\\');
                s.push(b[i + 1]);
                i += 2;
            }
            c => {
                s.push(c);
                i += 1;
            }
        }
    }
    Err(DslError(format!("line {line}: unterminated /regex/")))
}

// ---------------------------------------------------------------------------
// Parser
// ---------------------------------------------------------------------------

struct Parser {
    toks: Vec<(Tok, usize)>,
    pos: usize,
}

impl Parser {
    fn peek(&self) -> Option<&Tok> {
        self.toks.get(self.pos).map(|(t, _)| t)
    }
    fn line(&self) -> usize {
        self.toks
            .get(self.pos)
            .or_else(|| self.toks.last())
            .map(|(_, l)| *l)
            .unwrap_or(0)
    }
    fn bump(&mut self) -> Option<Tok> {
        let t = self.toks.get(self.pos).map(|(t, _)| t.clone());
        if t.is_some() {
            self.pos += 1;
        }
        t
    }
    fn at_kw(&self, s: &str) -> bool {
        matches!(self.peek(), Some(Tok::Ident(x)) if x == s)
    }
    fn eat_kw(&mut self, s: &str) -> R<()> {
        if self.at_kw(s) {
            self.pos += 1;
            Ok(())
        } else {
            Err(self.err(&format!("expected `{s}`")))
        }
    }
    fn expect(&mut self, t: &Tok) -> R<()> {
        if self.peek() == Some(t) {
            self.pos += 1;
            Ok(())
        } else {
            Err(self.err(&format!("expected {t:?}")))
        }
    }
    fn err(&self, msg: &str) -> DslError {
        DslError(format!("line {}: {msg}, found {:?}", self.line(), self.peek()))
    }
    fn ident(&mut self) -> R<String> {
        match self.bump() {
            Some(Tok::Ident(s)) => Ok(s),
            _ => Err(self.err("expected an identifier")),
        }
    }
    fn string(&mut self) -> R<String> {
        match self.bump() {
            Some(Tok::Str(s)) => Ok(s),
            _ => Err(self.err("expected a \"string\"")),
        }
    }

    fn parse_file(&mut self) -> R<RuleFile> {
        let mut rules = Vec::new();
        let mut waivers_dir = None;
        let mut adr_dir = None;
        while self.peek().is_some() {
            if self.at_kw("rule") {
                rules.push(self.parse_rule()?);
            } else if self.at_kw("waivers") {
                self.bump();
                waivers_dir = Some(self.string()?);
            } else if self.at_kw("adrs") {
                self.bump();
                adr_dir = Some(self.string()?);
            } else {
                return Err(self.err("expected `rule`, `waivers`, or `adrs`"));
            }
        }
        Ok(RuleFile {
            rules,
            waivers_dir,
            adr_dir,
        })
    }

    fn parse_rule(&mut self) -> R<RawRule> {
        self.eat_kw("rule")?;
        let id = self.ident()?;
        let mut severity = Severity::Error;
        if self.peek() == Some(&Tok::LParen) {
            self.bump();
            let sev = self.ident()?;
            severity = match sev.as_str() {
                "error" => Severity::Error,
                "warning" => Severity::Warning,
                other => return Err(self.err(&format!("unknown severity `{other}`"))),
            };
            self.expect(&Tok::RParen)?;
        }
        self.expect(&Tok::LBrace)?;

        let mut description = None;
        let mut message = None;
        let mut languages: Option<Vec<Language>> = None;
        let mut include: Vec<String> = Vec::new();
        let mut exclude: Vec<String> = Vec::new();
        let mut unless = RawUnless::default();
        let mut has_unless = false;
        let mut match_: Option<RawMatch> = None;
        let mut match_sequence: Option<RawSequence> = None;
        let mut where_scope: Option<RawWhereScope> = None;
        let mut compose: Option<RawCompose> = None;
        let mut count: Option<RawCount> = None;

        while self.peek() != Some(&Tok::RBrace) {
            if self.peek().is_none() {
                return Err(self.err("unexpected end of input inside `rule { }`"));
            }
            if self.at_kw("lang") {
                self.bump();
                languages = Some(self.parse_langs()?);
            } else if self.at_kw("in") {
                self.bump();
                include.extend(self.parse_str_list()?);
            } else if self.at_kw("not") {
                self.bump();
                self.eat_kw("in")?;
                exclude.extend(self.parse_str_list()?);
            } else if self.at_kw("desc") {
                self.bump();
                description = Some(self.string()?);
            } else if self.at_kw("message") {
                self.bump();
                message = Some(self.string()?);
            } else if self.at_kw("unless") {
                self.bump();
                self.parse_unless(&mut unless)?;
                has_unless = true;
            } else if self.at_kw("match") {
                self.bump();
                let attrs = self.parse_pattern()?;
                match_ = Some(RawMatch { attrs });
                if self.at_kw("where") {
                    where_scope = Some(self.parse_where_scope()?);
                }
            } else if self.at_kw("sequence") {
                self.bump();
                match_sequence = Some(self.parse_sequence()?);
            } else if self.at_kw("compose") {
                self.bump();
                compose = Some(self.parse_compose()?);
            } else if self.at_kw("count") {
                self.bump();
                count = Some(self.parse_count()?);
            } else {
                return Err(self.err("unexpected statement in rule body"));
            }
        }
        self.expect(&Tok::RBrace)?;

        let applies_to = if languages.is_some() || !include.is_empty() || !exclude.is_empty() {
            let paths = if include.is_empty() && exclude.is_empty() {
                None
            } else {
                Some(PathFilter { include, exclude })
            };
            Some(AppliesTo { languages, paths })
        } else {
            None
        };

        Ok(RawRule {
            id,
            severity,
            description,
            message,
            applies_to,
            match_,
            match_sequence,
            where_scope,
            compose,
            count,
            unless: if has_unless { Some(unless) } else { None },
        })
    }

    fn parse_langs(&mut self) -> R<Vec<Language>> {
        let mut out = Vec::new();
        loop {
            let name = self.ident()?;
            let lang: Language =
                serde_json::from_value(serde_json::Value::String(name.to_lowercase()))
                    .map_err(|_| self.err(&format!("unknown language `{name}`")))?;
            out.push(lang);
            if self.peek() == Some(&Tok::Comma) {
                self.bump();
            } else {
                break;
            }
        }
        Ok(out)
    }

    fn parse_str_list(&mut self) -> R<Vec<String>> {
        let mut out = vec![self.string()?];
        while self.peek() == Some(&Tok::Comma) {
            self.bump();
            out.push(self.string()?);
        }
        Ok(out)
    }

    fn parse_unless(&mut self, unless: &mut RawUnless) -> R<()> {
        if self.at_kw("path") {
            self.bump();
            let globs = self.parse_str_list()?;
            unless.path_matches = Some(globs);
        } else if self.at_kw("waiver") {
            self.bump();
            // optional explicit rule id
            let rule = if matches!(self.peek(), Some(Tok::Ident(_))) {
                Some(self.ident()?)
            } else {
                None
            };
            unless.waiver_exists = Some(WaiverGuard { rule });
        } else if self.at_kw("adr") {
            self.bump();
            let topic = self.string()?;
            unless.adr_exists = Some(AdrGuard { topic });
        } else {
            return Err(self.err("expected `path`, `waiver`, or `adr` after `unless`"));
        }
        Ok(())
    }

    /// One token atom: a `@class` matcher (`class = name`) or a `/regex/` text
    /// matcher (`text.regex = re`). Conjoined with `&` by the caller.
    fn token_atom(&mut self) -> R<Attrs> {
        let mut a = BTreeMap::new();
        match self.bump() {
            Some(Tok::TypeClass(name)) => {
                a.insert("class".to_string(), Value::String(name));
            }
            Some(Tok::Regex(re)) => {
                a.insert("text.regex".to_string(), Value::String(re));
            }
            _ => return Err(self.err("expected a `@class` or `/regex/` token matcher")),
        }
        Ok(a)
    }

    /// A token pattern: `Token[ pred, ... ]`, a `@class`/`/regex/` chain joined by
    /// `&`, or `any` (the wildcard — an empty predicate set matching any token).
    fn parse_pattern(&mut self) -> R<Attrs> {
        if self.at_kw("any") {
            self.bump();
            return Ok(BTreeMap::new());
        }
        // `@class` and/or `/regex/`, conjoined with `&` on one token.
        if matches!(self.peek(), Some(Tok::Regex(_)) | Some(Tok::TypeClass(_))) {
            let mut attrs = self.token_atom()?;
            while self.peek() == Some(&Tok::Amp) {
                self.bump();
                for (k, v) in self.token_atom()? {
                    attrs.insert(k, v);
                }
            }
            return Ok(attrs);
        }
        // Explicit field form: `Token[ pred, ... ]`.
        if self.at_kw("Token") {
            self.bump();
            self.expect(&Tok::LBrack)?;
            let mut attrs = BTreeMap::new();
            if self.peek() != Some(&Tok::RBrack) {
                loop {
                    let (k, v) = self.parse_pred()?;
                    attrs.insert(k, v);
                    if self.peek() == Some(&Tok::Comma) {
                        self.bump();
                    } else {
                        break;
                    }
                }
            }
            self.expect(&Tok::RBrack)?;
            return Ok(attrs);
        }
        Err(self.err("expected a token pattern: `Token[...]`, `@class`, `/regex/`, or `any`"))
    }

    /// One `field <op> <value>` predicate, returned as the (key, value) pair the
    /// compiler expects (operator encoded in the key suffix).
    fn parse_pred(&mut self) -> R<(String, Value)> {
        let attr = self.ident()?;
        // membership operator is the keyword `in`
        if self.at_kw("in") {
            self.bump();
            self.expect(&Tok::LBrack)?;
            let mut items = Vec::new();
            if self.peek() != Some(&Tok::RBrack) {
                loop {
                    items.push(Value::String(self.string()?));
                    if self.peek() == Some(&Tok::Comma) {
                        self.bump();
                    } else {
                        break;
                    }
                }
            }
            self.expect(&Tok::RBrack)?;
            return Ok((format!("{attr}.any"), Value::Sequence(items)));
        }
        let op = self.bump().ok_or_else(|| self.err("expected an operator"))?;
        match op {
            Tok::Eq => {
                let v = self.scalar_value()?;
                Ok((attr, Value::String(v)))
            }
            Tok::Tilde => Ok((format!("{attr}.regex"), Value::String(self.regex_value()?))),
            Tok::NotTilde => Ok((
                format!("{attr}.not.regex"),
                Value::String(self.regex_value()?),
            )),
            Tok::Gt => Ok((format!("{attr}.gt"), Value::String(self.num_value()?))),
            Tok::Lt => Ok((format!("{attr}.lt"), Value::String(self.num_value()?))),
            Tok::Ge => Ok((format!("{attr}.ge"), Value::String(self.num_value()?))),
            Tok::Le => Ok((format!("{attr}.le"), Value::String(self.num_value()?))),
            Tok::EqEq => Ok((format!("{attr}.eq_ref"), Value::String(self.var_value()?))),
            Tok::Ne => Ok((format!("{attr}.ne_ref"), Value::String(self.var_value()?))),
            other => Err(self.err(&format!("unexpected operator {other:?}"))),
        }
    }

    fn scalar_value(&mut self) -> R<String> {
        match self.bump() {
            Some(Tok::Str(s)) => Ok(s),
            Some(Tok::Num(s)) => Ok(s),
            _ => Err(self.err("expected a \"string\" or number")),
        }
    }
    fn num_value(&mut self) -> R<String> {
        match self.bump() {
            Some(Tok::Num(s)) => Ok(s),
            _ => Err(self.err("expected a number")),
        }
    }
    fn regex_value(&mut self) -> R<String> {
        match self.bump() {
            Some(Tok::Regex(s)) => Ok(s),
            _ => Err(self.err("expected a /regex/")),
        }
    }
    fn var_value(&mut self) -> R<String> {
        self.expect(&Tok::Dollar)?;
        self.ident()
    }

    fn parse_where_scope(&mut self) -> R<RawWhereScope> {
        let mut ws = RawWhereScope {
            contains: None,
            not_contains: None,
            followed_by: None,
        };
        while self.at_kw("where") {
            self.bump();
            self.eat_kw("scope")?;
            if self.at_kw("not") {
                self.bump();
                self.eat_kw("contains")?;
                ws.not_contains = Some(self.clause_match()?);
            } else if self.at_kw("contains") {
                self.bump();
                ws.contains = Some(self.clause_match()?);
            } else if self.at_kw("followed") {
                self.bump();
                self.eat_kw("by")?;
                ws.followed_by = Some(self.clause_match()?);
            } else {
                return Err(self.err("expected `contains`, `not contains`, or `followed by`"));
            }
        }
        Ok(ws)
    }

    fn clause_match(&mut self) -> R<RawMatch> {
        Ok(RawMatch {
            attrs: self.parse_pattern()?,
        })
    }

    fn parse_sequence(&mut self) -> R<RawSequence> {
        let anchor = if self.at_kw("in") {
            self.bump();
            self.eat_kw("scope")?;
            Some(RawAnchor {
                within: Some("ScopeStart..ScopeEnd".to_string()),
            })
        } else {
            None
        };
        self.expect(&Tok::LBrace)?;
        let mut steps = Vec::new();
        while self.peek() != Some(&Tok::RBrace) {
            if self.peek().is_none() {
                return Err(self.err("unexpected end of input inside `sequence { }`"));
            }
            steps.push(self.parse_step()?);
        }
        self.expect(&Tok::RBrace)?;
        Ok(RawSequence { anchor, steps })
    }

    fn parse_step(&mut self) -> R<RawStep> {
        let negate = if self.at_kw("not") {
            self.bump();
            true
        } else {
            false
        };

        let (attrs, alt) = if self.peek() == Some(&Tok::LParen) {
            // alternation of single token patterns
            self.bump();
            let mut alts: Vec<Vec<RawStep>> = Vec::new();
            loop {
                let a = self.parse_pattern()?;
                alts.push(vec![RawStep {
                    attrs: a,
                    quant: None,
                    negate: None,
                    bind: None,
                    alt: None,
                }]);
                if self.peek() == Some(&Tok::Pipe) {
                    self.bump();
                } else {
                    break;
                }
            }
            self.expect(&Tok::RParen)?;
            (BTreeMap::new(), Some(alts))
        } else {
            (self.parse_pattern()?, None)
        };

        let quant = match self.peek() {
            Some(Tok::Star) => {
                self.bump();
                Some("zero_or_more".to_string())
            }
            Some(Tok::Plus) => {
                self.bump();
                Some("one_or_more".to_string())
            }
            Some(Tok::Question) => {
                self.bump();
                Some("optional".to_string())
            }
            _ => None,
        };

        let mut binds = BTreeMap::new();
        if self.at_kw("as") {
            self.bump();
            binds = self.parse_bindlist()?;
        }

        Ok(RawStep {
            attrs,
            quant,
            negate: Some(negate),
            bind: if binds.is_empty() { None } else { Some(binds) },
            alt,
        })
    }

    fn parse_bindlist(&mut self) -> R<Binds> {
        let mut m = BTreeMap::new();
        loop {
            let var = self.ident()?;
            self.expect(&Tok::Eq)?;
            let attr = self.ident()?;
            m.insert(var, attr);
            if self.peek() == Some(&Tok::Comma) {
                self.bump();
            } else {
                break;
            }
        }
        Ok(m)
    }

    fn parse_compose(&mut self) -> R<RawCompose> {
        let op = self.ident()?;
        self.eat_kw("of")?;
        let mut of = vec![self.ident()?];
        while self.peek() == Some(&Tok::Comma) {
            self.bump();
            of.push(self.ident()?);
        }
        let mut key = Vec::new();
        if self.at_kw("by") {
            self.bump();
            key.push(self.ident()?);
            while self.peek() == Some(&Tok::Comma) {
                self.bump();
                key.push(self.ident()?);
            }
        }
        Ok(RawCompose { op, of, key })
    }

    fn parse_count(&mut self) -> R<RawCount> {
        let rule = self.ident()?;
        self.eat_kw("per")?;
        let scope = self.ident()?;
        let op = match self.bump() {
            Some(Tok::Gt) => "gt",
            Some(Tok::Lt) => "lt",
            Some(Tok::Ge) => "ge",
            Some(Tok::Le) => "le",
            Some(Tok::EqEq) => "eq",
            _ => return Err(self.err("expected a comparison (>, <, >=, <=, ==)")),
        }
        .to_string();
        let n: u64 = match self.bump() {
            Some(Tok::Num(s)) => s
                .parse()
                .map_err(|_| DslError(format!("line {}: count threshold must be an integer", self.line())))?,
            _ => return Err(self.err("expected an integer threshold")),
        };
        Ok(RawCount {
            rule,
            scope,
            op,
            n,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_single_token_rule() {
        let src = r#"
            rule NO_DEBUGGER (error) {
              lang typescript
              in "apps/*/src/**/*.{ts,tsx}"
              not in "**/generated/**"
              match Token[node_kind = "debugger"]
              message "Remove debugger statements."
            }
        "#;
        let (rules, _) = load(src).unwrap();
        assert_eq!(rules.len(), 1);
        let r = &rules[0];
        assert_eq!(r.id, "NO_DEBUGGER");
        assert_eq!(r.preds.len(), 1);
        assert!(matches!(r.preds[0], super::super::AttrPred::Eq { .. }));
        assert!(r.message.as_deref().unwrap().contains("debugger"));
    }

    #[test]
    fn parses_sequence_with_multibind_and_alt() {
        let src = r#"
            rule SEQ (warning) {
              lang typescript, javascript
              sequence in scope {
                Token[class = "ident"] as a = text, b = node_kind
                ( Token[node_kind = "=="] | Token[node_kind = "==="] )
              }
              message "x"
            }
        "#;
        let (rules, _) = load(src).unwrap();
        let seq = rules[0].sequence.as_ref().unwrap();
        assert!(seq.within_scope);
        assert_eq!(seq.steps.len(), 2);
        assert_eq!(seq.steps[0].bind.len(), 2, "first step binds two vars");
        assert!(matches!(seq.steps[1].matcher, super::super::StepMatcher::Alt(_)));
    }

    #[test]
    fn parses_compose_and_count() {
        let src = r#"
            rule A (error) { compose intersection of X, Y by file, function }
            rule B (error) { count Z per file > 10 }
        "#;
        let (rules, _) = load(src).unwrap();
        assert!(rules[0].compose.is_some());
        assert!(rules[1].count.is_some());
    }

    #[test]
    fn parses_where_scope() {
        let src = r#"
            rule ACQUIRE (warning) {
              match Token[text = "acquire"]
              where scope not contains Token[text = "release"]
            }
        "#;
        let (rules, _) = load(src).unwrap();
        let ws = rules[0].where_scope.as_ref().unwrap();
        assert!(ws.not_contains.is_some());
    }

    #[test]
    fn reports_a_line_number_on_error() {
        let src = "rule X (error) {\n  match\n}";
        let e = load(src).unwrap_err();
        assert!(e.0.contains("line"), "error should carry a line: {}", e.0);
    }

    #[test]
    fn at_class_and_amp_desugar_to_class_and_text() {
        // `@ident & /^pumba/` (Cobra) -> Token[class = "ident", text ~ /^pumba/].
        let (rules, _) = load(r#"rule L (warning) { match @ident & /^pumba/ }"#).unwrap();
        let r = &rules[0];
        assert!(
            r.preds.iter().any(|p| matches!(p, super::super::AttrPred::Eq { attr, value }
                if attr == "class" && value == "ident")),
            "@ident -> class = ident: {:?}",
            r.preds
        );
        assert!(
            r.preds.iter().any(|p| matches!(p, super::super::AttrPred::Regex { attr, re }
                if attr == "text" && re.as_str() == "^pumba")),
            "/^pumba/ -> text ~ /^pumba/: {:?}",
            r.preds
        );
    }

    #[test]
    fn bare_regex_is_a_text_regex_token_matcher() {
        let (rules, _) = load(r#"rule L (warning) { match /^pumba/ }"#).unwrap();
        let r = &rules[0];
        assert!(
            matches!(&r.preds[0], super::super::AttrPred::Regex { attr, re }
                if attr == "text" && re.as_str() == "^pumba"),
            "bare /re/ -> Token[text ~ /re/]: {:?}",
            r.preds
        );
    }

    #[test]
    fn star_after_a_token_is_repetition() {
        // `*` is token-level Kleene on a step, never a character glob.
        let (rules, _) = load(
            r#"rule L (warning) { sequence { Token[text = "log"] * Token[text = "save"] } }"#,
        )
        .unwrap();
        let seq = rules[0].sequence.as_ref().unwrap();
        assert_eq!(seq.steps.len(), 2);
        assert_eq!(
            seq.steps[0].quant,
            crate::Quant::ZeroOrMore,
            "the `log` lexeme step repeats"
        );
    }
}
