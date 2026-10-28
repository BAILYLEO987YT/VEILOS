// -----| VSH language core |-----
//
// Pure language implementation: lexer, parser and interpreter for VSH.
// This file has NO kernel dependencies (only `alloc`), so it can be unit
// tested on a normal host. The kernel glue lives in vscript.rs and plugs in
// through the `Host` trait.
//
// Design rules (see dark.vsh header):
//   * Deterministic: bounded steps, bounded recursion, no hidden mutation.
//   * Strict: undefined variables, type mismatches, bad indexes are errors
//     (never silently turned into strings / zero).
//   * Element definitions (`var X: element.button = {...}`) are LAZY: their
//     property expressions are stored unevaluated so the UI runtime can decide
//     which are live (re-evaluated on input change) and which are snapshots.
//     This core only stores them and hands them to the host on `add`.

use alloc::boxed::Box;
use alloc::collections::{BTreeMap, BTreeSet};
use alloc::format;
use alloc::sync::Arc;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

// =====================================================================
// -----| limits |-----
// =====================================================================

/// Maximum nesting of expressions while parsing (protects the kernel stack).
const MAX_PARSE_DEPTH: u32 = 48;
/// Maximum nested function/event/handler calls.
const MAX_CALL_DEPTH: usize = 24;
/// Maximum statements + calls executed per top-level entry (load / event).
const MAX_STEPS: u64 = 500_000;
/// Maximum nested `import` depth.
const MAX_IMPORT_DEPTH: usize = 8;

// =====================================================================
// -----| values |-----
// =====================================================================

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rgba {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub a: u8,
}

impl Rgba {
    pub const TRANSPARENT: Rgba = Rgba { r: 0, g: 0, b: 0, a: 0 };
}

/// A script-visible value.
#[derive(Debug, Clone, PartialEq)]
pub enum VValue {
    Integer(i64),
    Float(f64),
    String(String),
    Boolean(bool),
    Color(Rgba),
    Object(BTreeMap<String, VValue>),
    /// `index` typed variable (legacy): reset to 0 whenever an event fires.
    IndexVar(i64),
    Array(Vec<VValue>),
    /// A lazily evaluated element definition (see header).
    ElementDef(Arc<ElementDef>),
    /// A packaged event handler (function body or inline block).
    Handler(Handler),
    /// Opaque handle to a live element instance, owned by the host.
    Ref(u32),
    None,
}

/// Element definition: a kind plus UNEVALUATED property expressions.
#[derive(Debug, Clone, PartialEq)]
pub struct ElementDef {
    /// "" for the bare `element` type (a base that can be spread anywhere).
    pub kind: String,
    pub props: BTreeMap<String, Expr>,
}

/// An event handler packaged with an element at `add` time.
#[derive(Debug, Clone, PartialEq)]
pub enum Handler {
    Func(Arc<FuncDef>),
    Block(Arc<Vec<Stmt>>),
}

impl VValue {
    /// Human readable form used by `print`, string concatenation and `{}`.
    pub fn to_display(&self) -> String {
        match self {
            VValue::Integer(v) | VValue::IndexVar(v) => format!("{}", v),
            VValue::Float(f) => format!("{:?}", f),
            VValue::String(s) => s.clone(),
            VValue::Boolean(b) => if *b { "true".to_string() } else { "false".to_string() },
            VValue::Color(c) => {
                if c.a == 255 {
                    format!("#{:02x}{:02x}{:02x}", c.r, c.g, c.b)
                } else {
                    format!("#{:02x}{:02x}{:02x}{:02x}", c.r, c.g, c.b, c.a)
                }
            }
            VValue::Array(a) => {
                let mut s = String::from("[");
                for (i, v) in a.iter().enumerate() {
                    if i > 0 { s.push_str(", "); }
                    s.push_str(&v.to_display());
                }
                s.push(']');
                s
            }
            VValue::Object(m) => {
                let mut s = String::from("{");
                for (i, (k, v)) in m.iter().enumerate() {
                    if i > 0 { s.push_str(", "); }
                    s.push_str(k);
                    s.push_str(": ");
                    s.push_str(&v.to_display());
                }
                s.push('}');
                s
            }
            VValue::ElementDef(d) => {
                if d.kind.is_empty() { "<element>".to_string() } else { format!("<element.{}>", d.kind) }
            }
            VValue::Handler(_) => "<handler>".to_string(),
            VValue::Ref(id) => format!("<ref #{}>", id),
            VValue::None => String::new(),
        }
    }

    pub fn type_name(&self) -> &'static str {
        match self {
            VValue::Integer(_) => "int",
            VValue::Float(_) => "float",
            VValue::String(_) => "string",
            VValue::Boolean(_) => "bool",
            VValue::Color(_) => "color",
            VValue::Object(_) => "object",
            VValue::IndexVar(_) => "index",
            VValue::Array(_) => "list",
            VValue::ElementDef(_) => "element",
            VValue::Handler(_) => "handler",
            VValue::Ref(_) => "element_ref",
            VValue::None => "none",
        }
    }
}

// =====================================================================
// -----| declared types |-----
// =====================================================================

#[derive(Debug, Clone, PartialEq)]
pub enum Type {
    Int,
    Float,
    Str,
    Bool,
    List,
    Object,
    Color,
    Index,
    /// `element` (None) or `element.<kind>` (Some(kind)).
    Element(Option<String>),
    ElementRef,
}

impl Type {
    fn parse(name: &str, sub: Option<&str>) -> Result<Type, String> {
        match (name, sub) {
            ("int", None) => Ok(Type::Int),
            ("float", None) => Ok(Type::Float),
            ("string", None) => Ok(Type::Str),
            ("bool", None) => Ok(Type::Bool),
            ("list", None) => Ok(Type::List),
            ("object", None) => Ok(Type::Object),
            ("color", None) => Ok(Type::Color),
            ("index", None) => Ok(Type::Index),
            ("element_ref", None) => Ok(Type::ElementRef),
            ("element", None) => Ok(Type::Element(None)),
            ("element", Some(k)) => Ok(Type::Element(Some(k.to_string()))),
            (n, _) => Err(format!(
                "unknown type '{}' (expected int, float, string, bool, list, object, color, index, element[.kind], element_ref)",
                n
            )),
        }
    }

    fn describe(&self) -> String {
        match self {
            Type::Int => "int".to_string(),
            Type::Float => "float".to_string(),
            Type::Str => "string".to_string(),
            Type::Bool => "bool".to_string(),
            Type::List => "list".to_string(),
            Type::Object => "object".to_string(),
            Type::Color => "color".to_string(),
            Type::Index => "index".to_string(),
            Type::Element(None) => "element".to_string(),
            Type::Element(Some(k)) => format!("element.{}", k),
            Type::ElementRef => "element_ref".to_string(),
        }
    }

    fn default_value(&self) -> VValue {
        match self {
            Type::Int => VValue::Integer(0),
            Type::Float => VValue::Float(0.0),
            Type::Str => VValue::String(String::new()),
            Type::Bool => VValue::Boolean(false),
            Type::List => VValue::Array(Vec::new()),
            Type::Object => VValue::Object(BTreeMap::new()),
            Type::Color => VValue::Color(Rgba::TRANSPARENT),
            Type::Index => VValue::IndexVar(0),
            Type::Element(k) => VValue::ElementDef(Arc::new(ElementDef {
                kind: k.clone().unwrap_or_default(),
                props: BTreeMap::new(),
            })),
            Type::ElementRef => VValue::None,
        }
    }

    /// Check (and where lossless, convert) a value against a declared type.
    fn coerce(&self, v: VValue) -> Result<VValue, String> {
        let mismatch = |v: &VValue| -> String {
            format!("expected {}, got {}", self.describe(), v.type_name())
        };
        match (self, v) {
            (Type::Int, VValue::Integer(i)) => Ok(VValue::Integer(i)),
            (Type::Int, VValue::IndexVar(i)) => Ok(VValue::Integer(i)),
            (Type::Float, VValue::Float(f)) => Ok(VValue::Float(f)),
            (Type::Float, VValue::Integer(i)) => Ok(VValue::Float(i as f64)),
            (Type::Float, VValue::IndexVar(i)) => Ok(VValue::Float(i as f64)),
            (Type::Str, VValue::String(s)) => Ok(VValue::String(s)),
            (Type::Bool, VValue::Boolean(b)) => Ok(VValue::Boolean(b)),
            (Type::List, VValue::Array(a)) => Ok(VValue::Array(a)),
            (Type::Object, VValue::Object(o)) => Ok(VValue::Object(o)),
            (Type::Color, VValue::Color(c)) => Ok(VValue::Color(c)),
            (Type::Index, VValue::Integer(i)) | (Type::Index, VValue::IndexVar(i)) => Ok(VValue::IndexVar(i)),
            (Type::ElementRef, VValue::Ref(r)) => Ok(VValue::Ref(r)),
            (Type::ElementRef, VValue::None) => Ok(VValue::None),
            (Type::Element(want), VValue::ElementDef(d)) => {
                match want {
                    Some(k) if !d.kind.is_empty() && &d.kind != k => {
                        Err(format!("expected element.{}, got element.{}", k, d.kind))
                    }
                    _ => Ok(VValue::ElementDef(d)),
                }
            }
            (_, other) => Err(mismatch(&other)),
        }
    }
}

// =====================================================================
// -----| tokens |-----
// =====================================================================

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kw {
    Var, OnEvent, Set, If, Else, For, In, Function, Return, Break, Continue,
    Import, Add, Not, And, Or,
}

impl Kw {
    fn from_word(w: &str) -> Option<Kw> {
        Some(match w {
            "var" => Kw::Var,
            "on_event" => Kw::OnEvent,
            "set" => Kw::Set,
            "if" => Kw::If,
            "else" => Kw::Else,
            "for" => Kw::For,
            "in" => Kw::In,
            "function" => Kw::Function,
            "return" => Kw::Return,
            "break" => Kw::Break,
            "continue" => Kw::Continue,
            "import" => Kw::Import,
            "add" => Kw::Add,
            "not" => Kw::Not,
            "and" => Kw::And,
            "or" => Kw::Or,
            _ => return None,
        })
    }

    fn text(self) -> &'static str {
        match self {
            Kw::Var => "var", Kw::OnEvent => "on_event", Kw::Set => "set", Kw::If => "if",
            Kw::Else => "else", Kw::For => "for", Kw::In => "in", Kw::Function => "function",
            Kw::Return => "return", Kw::Break => "break", Kw::Continue => "continue",
            Kw::Import => "import", Kw::Add => "add", Kw::Not => "not", Kw::And => "and",
            Kw::Or => "or",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
enum StrPart {
    Lit(String),
    /// `{expr}` inside a string literal, already lexed.
    Interp(Vec<Token>),
}

#[derive(Debug, Clone, PartialEq)]
enum Tok {
    Ident(String),
    Kw(Kw),
    Str(Vec<StrPart>),
    Int(i64),
    Float(f64),
    Color(Rgba),
    Colon, Equal, EqEq, NotEq, Lt, Le, Gt, Ge,
    Plus, Minus, Star, Slash, Percent, Bang, AndAnd, OrOr,
    Semicolon, Comma, Dot, Ellipsis,
    LBrace, RBrace, LParen, RParen, LBracket, RBracket,
}

#[derive(Debug, Clone, PartialEq)]
struct Token {
    tok: Tok,
    line: u32,
}

fn tok_describe(t: &Tok) -> String {
    match t {
        Tok::Ident(n) => format!("'{}'", n),
        Tok::Kw(k) => format!("'{}'", k.text()),
        Tok::Str(_) => "a string".to_string(),
        Tok::Int(n) => format!("'{}'", n),
        Tok::Float(f) => format!("'{:?}'", f),
        Tok::Color(_) => "a color".to_string(),
        Tok::Colon => "':'".to_string(),
        Tok::Equal => "'='".to_string(),
        Tok::EqEq => "'=='".to_string(),
        Tok::NotEq => "'!='".to_string(),
        Tok::Lt => "'<'".to_string(),
        Tok::Le => "'<='".to_string(),
        Tok::Gt => "'>'".to_string(),
        Tok::Ge => "'>='".to_string(),
        Tok::Plus => "'+'".to_string(),
        Tok::Minus => "'-'".to_string(),
        Tok::Star => "'*'".to_string(),
        Tok::Slash => "'/'".to_string(),
        Tok::Percent => "'%'".to_string(),
        Tok::Bang => "'!'".to_string(),
        Tok::AndAnd => "'&&'".to_string(),
        Tok::OrOr => "'||'".to_string(),
        Tok::Semicolon => "';'".to_string(),
        Tok::Comma => "','".to_string(),
        Tok::Dot => "'.'".to_string(),
        Tok::Ellipsis => "'...'".to_string(),
        Tok::LBrace => "'{'".to_string(),
        Tok::RBrace => "'}'".to_string(),
        Tok::LParen => "'('".to_string(),
        Tok::RParen => "')'".to_string(),
        Tok::LBracket => "'['".to_string(),
        Tok::RBracket => "']'".to_string(),
    }
}

// =====================================================================
// -----| lexer |-----
// =====================================================================

struct Lexer<'a> {
    chars: core::iter::Peekable<core::str::Chars<'a>>,
    line: u32,
    depth: u32,
}

fn lex(input: &str) -> Result<Vec<Token>, String> {
    let mut lx = Lexer { chars: input.chars().peekable(), line: 1, depth: 0 };
    lx.run(1)
}

fn hex_val(c: char) -> u8 {
    c.to_digit(16).unwrap_or(0) as u8
}

impl<'a> Lexer<'a> {
    fn err<T>(&self, msg: &str) -> Result<T, String> {
        Err(format!("line {}: {}", self.line, msg))
    }

    fn run(&mut self, _first_line: u32) -> Result<Vec<Token>, String> {
        let mut tokens: Vec<Token> = Vec::new();
        while let Some(&c) = self.chars.peek() {
            if c == '\n' { self.line += 1; self.chars.next(); continue; }
            if c.is_whitespace() { self.chars.next(); continue; }
            let line = self.line;

            if c == '/' {
                self.chars.next();
                match self.chars.peek() {
                    Some(&'/') => {
                        while let Some(&ch) = self.chars.peek() {
                            if ch == '\n' { break; }
                            self.chars.next();
                        }
                    }
                    Some(&'*') => {
                        self.chars.next();
                        let mut prev = '\0';
                        let mut closed = false;
                        while let Some(ch) = self.chars.next() {
                            if ch == '\n' { self.line += 1; }
                            if prev == '*' && ch == '/' { closed = true; break; }
                            prev = ch;
                        }
                        if !closed { return self.err("unterminated /* comment"); }
                    }
                    _ => tokens.push(Token { tok: Tok::Slash, line }),
                }
                continue;
            }

            let tok = match c {
                ':' => { self.chars.next(); Tok::Colon }
                ';' => { self.chars.next(); Tok::Semicolon }
                ',' => { self.chars.next(); Tok::Comma }
                '{' => { self.chars.next(); Tok::LBrace }
                '}' => { self.chars.next(); Tok::RBrace }
                '(' => { self.chars.next(); Tok::LParen }
                ')' => { self.chars.next(); Tok::RParen }
                '[' => { self.chars.next(); Tok::LBracket }
                ']' => { self.chars.next(); Tok::RBracket }
                '+' => { self.chars.next(); Tok::Plus }
                '-' => { self.chars.next(); Tok::Minus }
                '*' => { self.chars.next(); Tok::Star }
                '%' => { self.chars.next(); Tok::Percent }
                '=' => {
                    self.chars.next();
                    if let Some(&'=') = self.chars.peek() { self.chars.next(); Tok::EqEq } else { Tok::Equal }
                }
                '!' => {
                    self.chars.next();
                    if let Some(&'=') = self.chars.peek() { self.chars.next(); Tok::NotEq } else { Tok::Bang }
                }
                '<' => {
                    self.chars.next();
                    if let Some(&'=') = self.chars.peek() { self.chars.next(); Tok::Le } else { Tok::Lt }
                }
                '>' => {
                    self.chars.next();
                    if let Some(&'=') = self.chars.peek() { self.chars.next(); Tok::Ge } else { Tok::Gt }
                }
                '&' => {
                    self.chars.next();
                    if let Some(&'&') = self.chars.peek() { self.chars.next(); Tok::AndAnd }
                    else { return self.err("unexpected '&' (did you mean '&&' or 'and'?)"); }
                }
                '|' => {
                    self.chars.next();
                    if let Some(&'|') = self.chars.peek() { self.chars.next(); Tok::OrOr }
                    else { return self.err("unexpected '|' (did you mean '||' or 'or'?)"); }
                }
                '.' => {
                    self.chars.next();
                    if let Some(&'.') = self.chars.peek() {
                        self.chars.next();
                        if let Some(&'.') = self.chars.peek() { self.chars.next(); Tok::Ellipsis }
                        else { return self.err("unexpected '..' (spread is '...')"); }
                    } else { Tok::Dot }
                }
                '#' => {
                    self.chars.next();
                    let mut hex = String::new();
                    while let Some(&ch) = self.chars.peek() {
                        if ch.is_ascii_hexdigit() { hex.push(ch); self.chars.next(); } else { break; }
                    }
                    let h: Vec<u8> = hex.chars().map(hex_val).collect();
                    match h.len() {
                        6 => Tok::Color(Rgba { r: h[0] * 16 + h[1], g: h[2] * 16 + h[3], b: h[4] * 16 + h[5], a: 255 }),
                        8 => Tok::Color(Rgba { r: h[0] * 16 + h[1], g: h[2] * 16 + h[3], b: h[4] * 16 + h[5], a: h[6] * 16 + h[7] }),
                        _ => return self.err(&format!("bad color '#{}' (use #rrggbb or #rrggbbaa)", hex)),
                    }
                }
                '"' => {
                    self.chars.next();
                    let parts = self.lex_string()?;
                    Tok::Str(parts)
                }
                _ if c.is_ascii_digit() => {
                    let mut n = String::new();
                    while let Some(&ch) = self.chars.peek() {
                        if ch.is_ascii_digit() { n.push(ch); self.chars.next(); } else { break; }
                    }
                    // A '.' only starts a fraction when followed by a digit
                    // (so `1.append` / `..` never get swallowed).
                    let mut is_float = false;
                    if let Some(&'.') = self.chars.peek() {
                        let mut look = self.chars.clone();
                        look.next();
                        if let Some(&d) = look.peek() {
                            if d.is_ascii_digit() {
                                is_float = true;
                                n.push('.');
                                self.chars.next();
                                while let Some(&ch) = self.chars.peek() {
                                    if ch.is_ascii_digit() { n.push(ch); self.chars.next(); } else { break; }
                                }
                            }
                        }
                    }
                    if is_float {
                        match n.parse::<f64>() {
                            Ok(f) => Tok::Float(f),
                            Err(_) => return self.err(&format!("bad number '{}'", n)),
                        }
                    } else {
                        match n.parse::<i64>() {
                            Ok(i) => Tok::Int(i),
                            Err(_) => return self.err(&format!("number '{}' is too large", n)),
                        }
                    }
                }
                _ if c.is_alphabetic() || c == '_' => {
                    let mut id = String::new();
                    while let Some(&ch) = self.chars.peek() {
                        if ch.is_alphanumeric() || ch == '_' { id.push(ch); self.chars.next(); } else { break; }
                    }
                    match Kw::from_word(&id) {
                        Some(k) => Tok::Kw(k),
                        None => Tok::Ident(id),
                    }
                }
                other => return self.err(&format!("unexpected character '{}'", other)),
            };
            tokens.push(Token { tok, line });
        }
        Ok(tokens)
    }

    /// Lex a string body (opening quote already consumed).
    /// Escapes: \" \\ \n \t \r \{ \}. Unknown escapes keep the backslash.
    /// `{expr}` starts an interpolation; the expression text is lexed here.
    fn lex_string(&mut self) -> Result<Vec<StrPart>, String> {
        let start_line = self.line;
        let mut parts: Vec<StrPart> = Vec::new();
        let mut cur = String::new();
        loop {
            let ch = match self.chars.next() {
                Some(c) => c,
                None => return Err(format!("line {}: unterminated string", start_line)),
            };
            match ch {
                '"' => break,
                '\n' => { self.line += 1; cur.push('\n'); }
                '\\' => {
                    match self.chars.next() {
                        Some('"') => cur.push('"'),
                        Some('\\') => cur.push('\\'),
                        Some('n') => cur.push('\n'),
                        Some('t') => cur.push('\t'),
                        Some('r') => cur.push('\r'),
                        Some('{') => cur.push('{'),
                        Some('}') => cur.push('}'),
                        Some(other) => { cur.push('\\'); cur.push(other); }
                        None => return Err(format!("line {}: unterminated string", start_line)),
                    }
                }
                '{' => {
                    if !cur.is_empty() {
                        parts.push(StrPart::Lit(core::mem::take(&mut cur)));
                    }
                    // Capture the raw expression text up to the matching '}'.
                    // Inside the braces, \" and \\ are unescaped so the
                    // expression can contain string literals of its own.
                    let mut depth = 1u32;
                    let mut expr_src = String::new();
                    let mut in_str = false;
                    loop {
                        let c2 = match self.chars.next() {
                            Some(c) => c,
                            None => return Err(format!("line {}: unterminated '{{' in string", start_line)),
                        };
                        if c2 == '\n' { self.line += 1; }
                        if c2 == '\\' {
                            match self.chars.next() {
                                Some('"') => { expr_src.push('"'); in_str = !in_str; }
                                Some('\\') => { expr_src.push('\\'); }
                                Some(o) => { expr_src.push('\\'); expr_src.push(o); }
                                None => return Err(format!("line {}: unterminated string", start_line)),
                            }
                            continue;
                        }
                        if !in_str {
                            if c2 == '{' { depth += 1; }
                            if c2 == '}' {
                                depth -= 1;
                                if depth == 0 { break; }
                            }
                        }
                        expr_src.push(c2);
                    }
                    if self.depth >= 4 {
                        return Err(format!("line {}: string interpolation nested too deeply", start_line));
                    }
                    let mut sub = Lexer { chars: expr_src.chars().peekable(), line: start_line, depth: self.depth + 1 };
                    let toks = sub.run(start_line)?;
                    if toks.is_empty() {
                        return Err(format!("line {}: empty '{{}}' in string", start_line));
                    }
                    parts.push(StrPart::Interp(toks));
                }
                other => cur.push(other),
            }
        }
        if !cur.is_empty() || parts.is_empty() {
            parts.push(StrPart::Lit(cur));
        }
        Ok(parts)
    }
}

// =====================================================================
// -----| AST |-----
// =====================================================================

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinOp { Add, Sub, Mul, Div, Rem, Eq, Ne, Lt, Le, Gt, Ge, And, Or }

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnOp { Neg, Not }

#[derive(Debug, Clone, PartialEq)]
pub struct Arg {
    pub name: Option<String>,
    pub value: Expr,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ObjEntry {
    Prop(String, Expr),
    Spread(Expr),
}

#[derive(Debug, Clone, PartialEq)]
pub enum InterpPart {
    Lit(String),
    Expr(Expr),
}

#[derive(Debug, Clone, PartialEq)]
pub enum Expr {
    Lit(VValue),
    Var(String),
    Object(Vec<ObjEntry>),
    Array(Vec<Expr>),
    Interp(Vec<InterpPart>),
    Unary(UnOp, Box<Expr>),
    Binary(BinOp, Box<Expr>, Box<Expr>),
    /// target[key]. A bare identifier key that is not a defined variable is
    /// the key NAME (so `THEME[accent]` works); use `.field` to always mean a name.
    Index(Box<Expr>, Box<Expr>),
    Field(Box<Expr>, String),
    Call(String, Vec<Arg>),
    Method(Box<Expr>, String, Vec<Arg>),
    /// `if(cond, a, b)` - only the chosen branch is evaluated.
    IfExpr(Box<Expr>, Box<Expr>, Box<Expr>),
    /// Legacy `{a}{b}` concatenation of formatted values.
    Concat(Vec<Expr>),
    Format(Box<Expr>),
    /// `add element.kind(Def, name: value, ...)`
    AddElement { kind: String, args: Vec<Arg> },
    /// Inline handler block (only as the value of an `on_*` property).
    Block(Arc<Vec<Stmt>>),
}

#[derive(Debug, Clone, PartialEq)]
pub enum PathSeg {
    Index(Expr),
    Field(String),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Target {
    pub name: String,
    pub path: Vec<PathSeg>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Stmt {
    pub kind: StmtKind,
    pub line: u32,
}

#[derive(Debug, Clone, PartialEq)]
pub enum StmtKind {
    VarDecl { name: String, ty: Option<Type>, value: Option<Expr> },
    Assign { target: Target, value: Expr },
    SetSystem { name: String, value: Expr },
    If { cond: Expr, then_body: Vec<Stmt>, else_body: Option<Vec<Stmt>> },
    For { var: String, iter: Expr, body: Vec<Stmt> },
    Return(Option<Expr>),
    Break,
    Continue,
    Expr(Expr),
    Import(String),
    /// Top level only; hoisted at load.
    Func(Arc<FuncDef>),
    /// Top level only; hoisted at load.
    Event(Arc<EventDef>),
}

#[derive(Debug, Clone, PartialEq)]
pub struct FuncDef {
    pub name: String,
    pub params: Vec<String>,
    pub body: Vec<Stmt>,
    pub line: u32,
}

#[derive(Debug, Clone, PartialEq)]
pub struct EventDef {
    pub name: String,
    /// Names bound from the event payload (`on_event window_opened(id, title)`).
    pub bindings: Vec<String>,
    /// `key = "VALUE"` filters (`on_event key_event(key = "ESC")`).
    pub filters: Vec<(String, String)>,
    pub body: Vec<Stmt>,
    pub line: u32,
}

// =====================================================================
// -----| parser |-----
// =====================================================================

struct Parser {
    toks: Vec<Token>,
    pos: usize,
    depth: u32,
    /// Nesting of blocks, so function/on_event/import are top-level only.
    block_depth: u32,
    last_line: u32,
}

fn parse_program(tokens: Vec<Token>) -> Result<Vec<Stmt>, String> {
    let last_line = tokens.last().map(|t| t.line).unwrap_or(1);
    let mut p = Parser { toks: tokens, pos: 0, depth: 0, block_depth: 0, last_line };
    let mut out = Vec::new();
    while !p.at_end() {
        out.push(p.parse_stmt()?);
    }
    Ok(out)
}

impl Parser {
    fn at_end(&self) -> bool { self.pos >= self.toks.len() }

    fn peek(&self) -> Option<&Tok> { self.toks.get(self.pos).map(|t| &t.tok) }

    fn peek_at(&self, off: usize) -> Option<&Tok> { self.toks.get(self.pos + off).map(|t| &t.tok) }

    fn line(&self) -> u32 {
        self.toks.get(self.pos).map(|t| t.line).unwrap_or(self.last_line)
    }

    fn err<T>(&self, msg: &str) -> Result<T, String> {
        Err(format!("line {}: {}", self.line(), msg))
    }

    fn next(&mut self) -> Option<Tok> {
        let t = self.toks.get(self.pos).map(|t| t.tok.clone());
        if t.is_some() { self.pos += 1; }
        t
    }

    fn check(&self, t: &Tok) -> bool { self.peek() == Some(t) }

    fn eat(&mut self, t: &Tok) -> bool {
        if self.check(t) { self.pos += 1; true } else { false }
    }

    fn expect(&mut self, t: &Tok, what: &str) -> Result<(), String> {
        if self.eat(t) { return Ok(()); }
        let found = match self.peek() {
            Some(x) => tok_describe(x),
            None => "end of file".to_string(),
        };
        self.err(&format!("expected {}, found {}", what, found))
    }

    fn eat_kw(&mut self, k: Kw) -> bool { self.eat(&Tok::Kw(k)) }

    fn expect_ident(&mut self, what: &str) -> Result<String, String> {
        match self.peek() {
            Some(Tok::Ident(n)) => { let n = n.clone(); self.pos += 1; Ok(n) }
            other => {
                let found = match other { Some(x) => tok_describe(x), None => "end of file".to_string() };
                self.err(&format!("expected {}, found {}", what, found))
            }
        }
    }

    /// Identifier or keyword used as a property name.
    fn key_name(&self) -> Option<String> {
        match self.peek() {
            Some(Tok::Ident(n)) => Some(n.clone()),
            Some(Tok::Kw(k)) => Some(k.text().to_string()),
            _ => None,
        }
    }

    fn skip_semi(&mut self) { self.eat(&Tok::Semicolon); }

    // ---------------- statements ----------------

    fn parse_block(&mut self) -> Result<Vec<Stmt>, String> {
        self.expect(&Tok::LBrace, "'{'")?;
        self.block_depth += 1;
        let mut body = Vec::new();
        while !self.at_end() && !self.check(&Tok::RBrace) {
            body.push(self.parse_stmt()?);
        }
        self.block_depth -= 1;
        self.expect(&Tok::RBrace, "'}'")?;
        Ok(body)
    }

    fn parse_stmt(&mut self) -> Result<Stmt, String> {
        let line = self.line();
        let kind = match self.peek() {
            Some(Tok::Kw(Kw::Var)) => self.parse_var()?,
            Some(Tok::Kw(Kw::Set)) => {
                self.pos += 1;
                let name = self.expect_ident("a system setting name after 'set'")?;
                self.expect(&Tok::Equal, "'='")?;
                let value = self.parse_expr()?;
                self.skip_semi();
                StmtKind::SetSystem { name, value }
            }
            Some(Tok::Kw(Kw::If)) if self.peek_at(1) != Some(&Tok::LParen) || !self.if_is_expression_stmt() => {
                self.parse_if()?
            }
            Some(Tok::Kw(Kw::For)) => {
                self.pos += 1;
                let var = self.expect_ident("a loop variable name")?;
                if !self.eat_kw(Kw::In) { return self.err("expected 'in' after the loop variable"); }
                let iter = self.parse_expr()?;
                let body = self.parse_block()?;
                self.skip_semi();
                StmtKind::For { var, iter, body }
            }
            Some(Tok::Kw(Kw::Return)) => {
                self.pos += 1;
                let value = if self.check(&Tok::Semicolon) || self.check(&Tok::RBrace) || self.at_end() {
                    None
                } else {
                    Some(self.parse_expr()?)
                };
                self.skip_semi();
                StmtKind::Return(value)
            }
            Some(Tok::Kw(Kw::Break)) => { self.pos += 1; self.skip_semi(); StmtKind::Break }
            Some(Tok::Kw(Kw::Continue)) => { self.pos += 1; self.skip_semi(); StmtKind::Continue }
            Some(Tok::Kw(Kw::Import)) => {
                if self.block_depth > 0 { return self.err("'import' is only allowed at the top level"); }
                self.pos += 1;
                let path = match self.next() {
                    Some(Tok::Str(parts)) if parts.len() == 1 => match &parts[0] {
                        StrPart::Lit(s) => s.clone(),
                        _ => return self.err("import path must be a plain string"),
                    },
                    _ => return self.err("expected a string path after 'import'"),
                };
                self.skip_semi();
                StmtKind::Import(path)
            }
            Some(Tok::Kw(Kw::Function)) => {
                if self.block_depth > 0 { return self.err("'function' is only allowed at the top level"); }
                self.pos += 1;
                let name = self.expect_ident("a function name")?;
                self.expect(&Tok::LParen, "'('")?;
                let mut params = Vec::new();
                while !self.check(&Tok::RParen) {
                    params.push(self.expect_ident("a parameter name")?);
                    if !self.eat(&Tok::Comma) { break; }
                }
                self.expect(&Tok::RParen, "')'")?;
                let body = self.parse_block()?;
                self.skip_semi();
                StmtKind::Func(Arc::new(FuncDef { name, params, body, line }))
            }
            Some(Tok::Kw(Kw::OnEvent)) => {
                if self.block_depth > 0 { return self.err("'on_event' is only allowed at the top level"); }
                self.pos += 1;
                let name = self.expect_ident("an event name")?;
                let mut bindings = Vec::new();
                let mut filters = Vec::new();
                if self.eat(&Tok::LParen) {
                    while !self.check(&Tok::RParen) {
                        let k = self.expect_ident("an event parameter name")?;
                        if self.eat(&Tok::Equal) {
                            match self.next() {
                                Some(Tok::Str(parts)) if parts.len() == 1 => match &parts[0] {
                                    StrPart::Lit(s) => filters.push((k, s.clone())),
                                    _ => return self.err("event filter value must be a plain string"),
                                },
                                _ => return self.err("expected a string after '=' in an event filter"),
                            }
                        } else {
                            bindings.push(k);
                        }
                        if !self.eat(&Tok::Comma) { break; }
                    }
                    self.expect(&Tok::RParen, "')'")?;
                }
                let body = self.parse_block()?;
                self.skip_semi();
                StmtKind::Event(Arc::new(EventDef { name, bindings, filters, body, line }))
            }
            Some(Tok::LBrace) => return self.err("unexpected '{' (a bare block is not a statement)"),
            Some(_) => {
                // Assignment or expression statement.
                let e = self.parse_expr()?;
                if self.eat(&Tok::Equal) {
                    let target = match expr_to_target(&e) {
                        Some(t) => t,
                        None => return self.err("left side of '=' is not assignable"),
                    };
                    let value = self.parse_expr()?;
                    self.skip_semi();
                    StmtKind::Assign { target, value }
                } else {
                    self.skip_semi();
                    StmtKind::Expr(e)
                }
            }
            None => return self.err("unexpected end of file"),
        };
        Ok(Stmt { kind, line })
    }

    /// `if(` at statement start is a statement when followed (after the
    /// matching ')') by '{', otherwise an expression statement like
    /// `if(a, b, c);`. Looks ahead without consuming.
    fn if_is_expression_stmt(&self) -> bool {
        // pos is at `if`, pos+1 is '('. Find the matching ')'.
        let mut depth = 0i32;
        let mut i = self.pos + 1;
        while i < self.toks.len() {
            match &self.toks[i].tok {
                Tok::LParen => depth += 1,
                Tok::RParen => {
                    depth -= 1;
                    if depth == 0 {
                        return !matches!(self.toks.get(i + 1).map(|t| &t.tok), Some(Tok::LBrace));
                    }
                }
                _ => {}
            }
            i += 1;
        }
        false
    }

    fn parse_if(&mut self) -> Result<StmtKind, String> {
        self.pos += 1; // if
        let cond = self.parse_expr()?;
        let then_body = self.parse_block()?;
        let else_body = if self.eat_kw(Kw::Else) {
            if self.check(&Tok::Kw(Kw::If)) {
                let line = self.line();
                let kind = self.parse_if()?;
                Some(alloc::vec![Stmt { kind, line }])
            } else {
                Some(self.parse_block()?)
            }
        } else {
            None
        };
        self.skip_semi();
        Ok(StmtKind::If { cond, then_body, else_body })
    }

    fn parse_var(&mut self) -> Result<StmtKind, String> {
        self.pos += 1; // var
        let name = self.expect_ident("a variable name")?;
        let mut ty = None;
        if self.eat(&Tok::Colon) {
            let t_name = self.expect_ident("a type name")?;
            let mut sub = None;
            if self.eat(&Tok::Dot) {
                sub = Some(self.expect_ident("an element kind after 'element.'")?);
            }
            ty = Some(Type::parse(&t_name, sub.as_deref()).map_err(|e| format!("line {}: {}", self.line(), e))?);
        }
        let value = if self.eat(&Tok::Equal) { Some(self.parse_expr()?) } else { None };
        if ty.is_none() && value.is_none() {
            return self.err(&format!("'var {}' needs a type or a value", name));
        }
        self.skip_semi();
        Ok(StmtKind::VarDecl { name, ty, value })
    }

    // ---------------- expressions ----------------

    fn parse_expr(&mut self) -> Result<Expr, String> {
        self.depth += 1;
        if self.depth > MAX_PARSE_DEPTH {
            self.depth -= 1;
            return self.err("expression nested too deeply");
        }
        let r = self.parse_or();
        self.depth -= 1;
        r
    }

    fn parse_or(&mut self) -> Result<Expr, String> {
        let mut lhs = self.parse_and()?;
        loop {
            if self.eat_kw(Kw::Or) || self.eat(&Tok::OrOr) {
                let rhs = self.parse_and()?;
                lhs = Expr::Binary(BinOp::Or, Box::new(lhs), Box::new(rhs));
            } else { break; }
        }
        Ok(lhs)
    }

    fn parse_and(&mut self) -> Result<Expr, String> {
        let mut lhs = self.parse_equality()?;
        loop {
            if self.eat_kw(Kw::And) || self.eat(&Tok::AndAnd) {
                let rhs = self.parse_equality()?;
                lhs = Expr::Binary(BinOp::And, Box::new(lhs), Box::new(rhs));
            } else { break; }
        }
        Ok(lhs)
    }

    fn parse_equality(&mut self) -> Result<Expr, String> {
        let mut lhs = self.parse_comparison()?;
        loop {
            let op = if self.eat(&Tok::EqEq) { BinOp::Eq }
                     else if self.eat(&Tok::NotEq) { BinOp::Ne }
                     else { break };
            let rhs = self.parse_comparison()?;
            lhs = Expr::Binary(op, Box::new(lhs), Box::new(rhs));
        }
        Ok(lhs)
    }

    fn parse_comparison(&mut self) -> Result<Expr, String> {
        let mut lhs = self.parse_additive()?;
        loop {
            let op = if self.eat(&Tok::Lt) { BinOp::Lt }
                     else if self.eat(&Tok::Le) { BinOp::Le }
                     else if self.eat(&Tok::Gt) { BinOp::Gt }
                     else if self.eat(&Tok::Ge) { BinOp::Ge }
                     else { break };
            let rhs = self.parse_additive()?;
            lhs = Expr::Binary(op, Box::new(lhs), Box::new(rhs));
        }
        Ok(lhs)
    }

    fn parse_additive(&mut self) -> Result<Expr, String> {
        let mut lhs = self.parse_multiplicative()?;
        loop {
            let op = if self.eat(&Tok::Plus) { BinOp::Add }
                     else if self.eat(&Tok::Minus) { BinOp::Sub }
                     else { break };
            let rhs = self.parse_multiplicative()?;
            lhs = Expr::Binary(op, Box::new(lhs), Box::new(rhs));
        }
        Ok(lhs)
    }

    fn parse_multiplicative(&mut self) -> Result<Expr, String> {
        let mut lhs = self.parse_unary()?;
        loop {
            let op = if self.eat(&Tok::Star) { BinOp::Mul }
                     else if self.eat(&Tok::Slash) { BinOp::Div }
                     else if self.eat(&Tok::Percent) { BinOp::Rem }
                     else { break };
            let rhs = self.parse_unary()?;
            lhs = Expr::Binary(op, Box::new(lhs), Box::new(rhs));
        }
        Ok(lhs)
    }

    fn parse_unary(&mut self) -> Result<Expr, String> {
        if self.eat(&Tok::Minus) {
            self.depth += 1;
            if self.depth > MAX_PARSE_DEPTH { self.depth -= 1; return self.err("expression nested too deeply"); }
            let e = self.parse_unary();
            self.depth -= 1;
            return Ok(Expr::Unary(UnOp::Neg, Box::new(e?)));
        }
        if self.eat_kw(Kw::Not) || self.eat(&Tok::Bang) {
            self.depth += 1;
            if self.depth > MAX_PARSE_DEPTH { self.depth -= 1; return self.err("expression nested too deeply"); }
            let e = self.parse_unary();
            self.depth -= 1;
            return Ok(Expr::Unary(UnOp::Not, Box::new(e?)));
        }
        self.parse_postfix()
    }

    fn parse_args(&mut self) -> Result<Vec<Arg>, String> {
        // '(' already consumed
        let mut args = Vec::new();
        while !self.check(&Tok::RParen) {
            if self.at_end() { return self.err("expected ')'"); }
            let named = matches!(self.peek(), Some(Tok::Ident(_))) && self.peek_at(1) == Some(&Tok::Colon);
            if named {
                let name = self.expect_ident("an argument name")?;
                self.pos += 1; // ':'
                let value = self.parse_expr()?;
                args.push(Arg { name: Some(name), value });
            } else {
                let value = self.parse_expr()?;
                args.push(Arg { name: None, value });
            }
            if !self.eat(&Tok::Comma) { break; }
        }
        self.expect(&Tok::RParen, "')'")?;
        Ok(args)
    }

    fn parse_postfix(&mut self) -> Result<Expr, String> {
        let mut e = self.parse_primary()?;
        loop {
            if self.eat(&Tok::LBracket) {
                let key = self.parse_expr()?;
                self.expect(&Tok::RBracket, "']'")?;
                e = Expr::Index(Box::new(e), Box::new(key));
            } else if self.check(&Tok::Dot) {
                self.pos += 1;
                let name = match self.key_name() {
                    Some(n) => { self.pos += 1; n }
                    None => return self.err("expected a property or method name after '.'"),
                };
                if self.eat(&Tok::LParen) {
                    let args = self.parse_args()?;
                    e = Expr::Method(Box::new(e), name, args);
                } else {
                    e = Expr::Field(Box::new(e), name);
                }
            } else {
                break;
            }
        }
        Ok(e)
    }

    fn parse_string(&mut self, parts: Vec<StrPart>) -> Result<Expr, String> {
        if parts.len() == 1 {
            if let StrPart::Lit(s) = &parts[0] {
                return Ok(Expr::Lit(VValue::String(s.clone())));
            }
        }
        let mut out = Vec::new();
        for p in parts {
            match p {
                StrPart::Lit(s) => out.push(InterpPart::Lit(s)),
                StrPart::Interp(toks) => {
                    let last_line = toks.last().map(|t| t.line).unwrap_or(self.line());
                    let mut sub = Parser { toks, pos: 0, depth: self.depth, block_depth: 0, last_line };
                    let e = sub.parse_expr()?;
                    if !sub.at_end() {
                        return sub.err("unexpected tokens after the expression inside '{...}'");
                    }
                    out.push(InterpPart::Expr(e));
                }
            }
        }
        Ok(Expr::Interp(out))
    }

    fn parse_primary(&mut self) -> Result<Expr, String> {
        let line = self.line();
        let tok = match self.next() {
            Some(t) => t,
            None => return Err(format!("line {}: unexpected end of file in expression", line)),
        };
        match tok {
            Tok::Int(n) => Ok(Expr::Lit(VValue::Integer(n))),
            Tok::Float(f) => Ok(Expr::Lit(VValue::Float(f))),
            Tok::Color(c) => Ok(Expr::Lit(VValue::Color(c))),
            Tok::Str(parts) => self.parse_string(parts),
            Tok::Kw(Kw::If) => {
                // expression form: if(cond, a, b)
                self.expect(&Tok::LParen, "'(' after 'if' in an expression")?;
                let c = self.parse_expr()?;
                self.expect(&Tok::Comma, "',' (if(cond, then, else))")?;
                let a = self.parse_expr()?;
                self.expect(&Tok::Comma, "',' (if(cond, then, else))")?;
                let b = self.parse_expr()?;
                self.expect(&Tok::RParen, "')'")?;
                Ok(Expr::IfExpr(Box::new(c), Box::new(a), Box::new(b)))
            }
            Tok::Kw(Kw::Add) => {
                let ns = self.expect_ident("'element' after 'add'")?;
                if ns != "element" {
                    return Err(format!("line {}: expected 'element.<kind>' after 'add', found '{}'", line, ns));
                }
                self.expect(&Tok::Dot, "'.' (add element.<kind>(...))")?;
                let kind = self.expect_ident("an element kind")?;
                self.expect(&Tok::LParen, "'('")?;
                let args = self.parse_args()?;
                Ok(Expr::AddElement { kind, args })
            }
            Tok::Ident(name) => {
                match name.as_str() {
                    "true" | "True" => return Ok(Expr::Lit(VValue::Boolean(true))),
                    "false" | "False" => return Ok(Expr::Lit(VValue::Boolean(false))),
                    _ => {}
                }
                if self.eat(&Tok::LParen) {
                    let args = self.parse_args()?;
                    Ok(Expr::Call(name, args))
                } else {
                    Ok(Expr::Var(name))
                }
            }
            Tok::LParen => {
                let mut items = Vec::new();
                let mut saw_comma = false;
                while !self.check(&Tok::RParen) {
                    if self.at_end() { return self.err("expected ')'"); }
                    items.push(self.parse_expr()?);
                    if self.eat(&Tok::Comma) { saw_comma = true; } else { break; }
                }
                self.expect(&Tok::RParen, "')'")?;
                if items.len() == 1 && !saw_comma {
                    Ok(items.pop().unwrap())
                } else {
                    Ok(Expr::Array(items))
                }
            }
            Tok::LBracket => {
                let mut items = Vec::new();
                while !self.check(&Tok::RBracket) {
                    if self.at_end() { return self.err("expected ']'"); }
                    items.push(self.parse_expr()?);
                    if !self.eat(&Tok::Comma) { break; }
                }
                self.expect(&Tok::RBracket, "']'")?;
                Ok(Expr::Array(items))
            }
            Tok::LBrace => self.parse_brace(),
            other => Err(format!("line {}: unexpected {} in expression", line, tok_describe(&other))),
        }
    }

    /// '{' already consumed: object literal or legacy `{a}{b}` concatenation.
    fn parse_brace(&mut self) -> Result<Expr, String> {
        let is_object = self.check(&Tok::RBrace)
            || self.check(&Tok::Ellipsis)
            || (self.key_name().is_some() && self.peek_at(1) == Some(&Tok::Colon));
        if is_object {
            let mut entries = Vec::new();
            while !self.check(&Tok::RBrace) {
                if self.at_end() { return self.err("expected '}'"); }
                if self.eat(&Tok::Ellipsis) {
                    let e = self.parse_unary_for_spread()?;
                    entries.push(ObjEntry::Spread(e));
                } else {
                    let key = match self.key_name() {
                        Some(k) => { self.pos += 1; k }
                        None => return self.err("expected a property name"),
                    };
                    self.expect(&Tok::Colon, "':' after the property name")?;
                    // `on_*` properties may hold an inline handler block.
                    let value = if key.starts_with("on_") && self.check(&Tok::LBrace) {
                        let body = self.parse_block()?;
                        Expr::Block(Arc::new(body))
                    } else {
                        self.parse_expr()?
                    };
                    entries.push(ObjEntry::Prop(key, value));
                }
                if !self.eat(&Tok::Comma) { break; }
            }
            self.expect(&Tok::RBrace, "'}'")?;
            Ok(Expr::Object(entries))
        } else {
            let first = self.parse_expr()?;
            self.expect(&Tok::RBrace, "'}'")?;
            let mut seq = alloc::vec![Expr::Format(Box::new(first))];
            while self.check(&Tok::LBrace) {
                self.pos += 1;
                let e = self.parse_expr()?;
                self.expect(&Tok::RBrace, "'}'")?;
                seq.push(Expr::Format(Box::new(e)));
            }
            if seq.len() == 1 { Ok(seq.pop().unwrap()) } else { Ok(Expr::Concat(seq)) }
        }
    }

    fn parse_unary_for_spread(&mut self) -> Result<Expr, String> {
        // A spread operand is a postfix expression (variable, field, call...).
        self.parse_postfix()
    }
}

/// Turn a parsed expression into an assignment target, if it is a variable
/// optionally followed by `.field` / `[index]` accessors.
fn expr_to_target(e: &Expr) -> Option<Target> {
    fn walk(e: &Expr, path: &mut Vec<PathSeg>) -> Option<String> {
        match e {
            Expr::Var(n) => Some(n.clone()),
            Expr::Field(inner, f) => {
                let n = walk(inner, path)?;
                path.push(PathSeg::Field(f.clone()));
                Some(n)
            }
            Expr::Index(inner, k) => {
                let n = walk(inner, path)?;
                path.push(PathSeg::Index((**k).clone()));
                Some(n)
            }
            _ => None,
        }
    }
    let mut path = Vec::new();
    let name = walk(e, &mut path)?;
    Some(Target { name, path })
}

// =====================================================================
// -----| host interface |-----
// =====================================================================

/// An evaluated call argument.
#[derive(Debug, Clone, PartialEq)]
pub struct CallArg {
    pub name: Option<String>,
    pub value: VValue,
}

/// Services the interpreter needs from the kernel.
pub trait Host {
    /// Apply a `set NAME = value;` system setting. Err for unknown names or
    /// invalid values.
    fn set_system(&mut self, name: &str, value: &VValue) -> Result<(), String>;

    /// Call a host function (print, launch, ...). `None` = no such function.
    fn call(&mut self, name: &str, args: &[CallArg]) -> Option<Result<VValue, String>>;

    /// Instantiate an element. `named` are the extra `name: value` arguments
    /// given to `add`; `snapshot` holds the definition's snapshot properties
    /// (every key `snapshot_key` accepts, plus all `on_*` handlers) evaluated
    /// at the moment of `add`, each with its own error. Returns the handle.
    fn add_element(
        &mut self,
        kind: &str,
        def: &ElementDef,
        named: &[CallArg],
        snapshot: &[(String, Result<VValue, String>)],
    ) -> Result<VValue, String> {
        let _ = (kind, def, named, snapshot);
        Err("elements are not available yet".to_string())
    }

    /// Whether a definition property is baked at `add` (snapshot) rather than
    /// re-evaluated when its inputs change (live). `on_*` handlers are always
    /// snapshot and are not asked about.
    fn snapshot_key(&self, key: &str) -> bool {
        let _ = key;
        false
    }

    /// Read a property of a live element (`ref[window_id]`).
    fn element_prop(&mut self, id: u32, key: &str) -> Result<VValue, String> {
        let _ = (id, key);
        Err("elements are not available yet".to_string())
    }

    /// Whether `kind` is a valid element kind (`element.<kind>`).
    fn is_element_kind(&self, kind: &str) -> bool {
        let _ = kind;
        true
    }

    /// Read a script file for `import`.
    fn read_file(&mut self, path: &str) -> Result<String, String> {
        let _ = path;
        Err("import is not available".to_string())
    }
}

// =====================================================================
// -----| interpreter |-----
// =====================================================================

struct Binding {
    value: VValue,
    ty: Option<Type>,
}

struct Frame {
    scopes: Vec<BTreeMap<String, Binding>>,
}

enum Flow {
    Normal,
    Break,
    Continue,
    Return(VValue),
}

#[derive(Default)]
struct SourceInfo {
    functions: Vec<String>,
    event_count: usize,
    globals: Vec<String>,
    imports: Vec<String>,
}

pub struct Interp {
    /// Global variables. Public so the kernel can read script settings
    /// (e.g. MESH_OFFLOAD_POLICY).
    pub variables: BTreeMap<String, VValue>,
    global_types: BTreeMap<String, Type>,
    functions: BTreeMap<String, (String, Arc<FuncDef>)>,
    events: Vec<(String, Arc<EventDef>)>,
    sources: BTreeMap<String, SourceInfo>,
    frames: Vec<Frame>,
    source_stack: Vec<String>,
    /// Names whose `var` declaration failed during the current load, so later
    /// "undefined variable" errors can say why.
    failed_decls: BTreeSet<String>,
    steps: u64,
    anon_counter: u64,
    /// Bumped on every variable write, `set`, load and unload. The UI runtime
    /// re-evaluates live properties only when this changes (nothing is polled).
    pub generation: u64,
}

const CORE_FUNCTIONS: [&str; 2] = ["format", "len"];

fn numeric(v: &VValue) -> Option<f64> {
    match v {
        VValue::Integer(i) | VValue::IndexVar(i) => Some(*i as f64),
        VValue::Float(f) => Some(*f),
        _ => None,
    }
}

fn int_of(v: &VValue) -> Option<i64> {
    match v {
        VValue::Integer(i) | VValue::IndexVar(i) => Some(*i),
        _ => None,
    }
}

fn values_equal(a: &VValue, b: &VValue) -> bool {
    match (a, b) {
        (VValue::Integer(_) | VValue::IndexVar(_) | VValue::Float(_),
         VValue::Integer(_) | VValue::IndexVar(_) | VValue::Float(_)) => {
            match (int_of(a), int_of(b)) {
                (Some(x), Some(y)) => x == y,
                _ => numeric(a) == numeric(b),
            }
        }
        (VValue::Array(x), VValue::Array(y)) => {
            x.len() == y.len() && x.iter().zip(y.iter()).all(|(p, q)| values_equal(p, q))
        }
        (VValue::Object(x), VValue::Object(y)) => {
            x.len() == y.len()
                && x.iter().all(|(k, v)| y.get(k).map_or(false, |w| values_equal(v, w)))
        }
        _ => a == b,
    }
}

fn truthy(v: &VValue) -> Result<bool, String> {
    match v {
        VValue::Boolean(b) => Ok(*b),
        VValue::Integer(i) | VValue::IndexVar(i) => Ok(*i != 0),
        VValue::Float(f) => Ok(*f != 0.0),
        VValue::None => Ok(false),
        other => Err(format!("a condition must be a bool or number, got {}", other.type_name())),
    }
}

impl Interp {
    pub fn new() -> Self {
        Interp {
            variables: BTreeMap::new(),
            global_types: BTreeMap::new(),
            functions: BTreeMap::new(),
            events: Vec::new(),
            sources: BTreeMap::new(),
            frames: Vec::new(),
            source_stack: Vec::new(),
            failed_decls: BTreeSet::new(),
            steps: 0,
            anon_counter: 0,
            generation: 0,
        }
    }

    // ---------------- public API ----------------

    /// Run a script under an anonymous source id (terminal one-liners).
    /// Handlers it registers accumulate, exactly like the old engine.
    pub fn run_anonymous(&mut self, text: &str, host: &mut dyn Host) -> Vec<String> {
        self.anon_counter += 1;
        let id = format!("<anon:{}>", self.anon_counter);
        self.load(&id, text, host)
    }

    /// Load (or hot-reload) a named script. If the source was loaded before,
    /// its functions, event handlers, globals and imports are removed first -
    /// but only after the new text parsed successfully.
    pub fn load(&mut self, source: &str, text: &str, host: &mut dyn Host) -> Vec<String> {
        self.generation = self.generation.wrapping_add(1);
        let tokens = match lex(text) {
            Ok(t) => t,
            Err(e) => return alloc::vec![format!("LEX ERROR: {}", e)],
        };
        let program = match parse_program(tokens) {
            Ok(p) => p,
            Err(e) => return alloc::vec![format!("PARSE ERROR: {}", e)],
        };
        if self.sources.contains_key(source) {
            self.unload(source);
        }
        self.steps = 0;
        self.failed_decls.clear();
        let mut errors = Vec::new();
        self.run_program(source, &program, host, &mut errors);
        // Drop empty anonymous sources so one-liners don't pile up.
        if source.starts_with("<anon:") {
            let empty = self.sources.get(source).map_or(true, |s| {
                s.functions.is_empty() && s.event_count == 0 && s.globals.is_empty() && s.imports.is_empty()
            });
            if empty { self.sources.remove(source); }
        }
        errors
    }

    /// Remove everything a source (and the sources it imported) registered.
    pub fn unload(&mut self, source: &str) {
        self.generation = self.generation.wrapping_add(1);
        let info = match self.sources.remove(source) {
            Some(i) => i,
            None => return,
        };
        self.functions.retain(|_, (src, _)| src != source);
        self.events.retain(|(src, _)| src != source);
        for g in &info.globals {
            self.variables.remove(g);
            self.global_types.remove(g);
        }
        for imp in &info.imports {
            // A shared import stays loaded while another source still uses it.
            let still_used = self.sources.values().any(|s| s.imports.iter().any(|i| i == imp));
            if !still_used {
                self.unload(imp);
            }
        }
    }

    /// Fire an event. `filters_in` are string filters (`key = "ESC"`),
    /// `payload` provides values for `on_event name(a, b)` bindings.
    pub fn trigger(
        &mut self,
        name: &str,
        filters_in: Option<&BTreeMap<String, String>>,
        payload: &BTreeMap<String, VValue>,
        host: &mut dyn Host,
    ) -> Vec<String> {
        self.trigger_impl(None, name, filters_in, payload, host)
    }

    /// Fire an event only for the handlers one source registered (used to
    /// re-run a reloaded shell's `start` without re-running every other
    /// script's `start`).
    pub fn trigger_source(
        &mut self,
        source: &str,
        name: &str,
        payload: &BTreeMap<String, VValue>,
        host: &mut dyn Host,
    ) -> Vec<String> {
        self.trigger_impl(Some(source), name, None, payload, host)
    }

    fn trigger_impl(
        &mut self,
        only_source: Option<&str>,
        name: &str,
        filters_in: Option<&BTreeMap<String, String>>,
        payload: &BTreeMap<String, VValue>,
        host: &mut dyn Host,
    ) -> Vec<String> {
        let mut errors = Vec::new();
        self.steps = 0;

        // Legacy: `index` variables reset to 0 whenever an event fires.
        for v in self.variables.values_mut() {
            if let VValue::IndexVar(x) = v { *x = 0; }
        }

        let handlers: Vec<Arc<EventDef>> = self.events.iter()
            .filter(|(src, e)| e.name == name && only_source.map_or(true, |o| o == src))
            .map(|(_, e)| e.clone())
            .collect();

        for ev in handlers {
            // Filters
            let mut ok = true;
            if !ev.filters.is_empty() {
                for (k, expected) in &ev.filters {
                    let actual = match filters_in.and_then(|f| f.get(k)) {
                        Some(v) => Some(v.clone()),
                        None => payload.get(k).map(|v| v.to_display()),
                    };
                    if actual.as_deref() != Some(expected.as_str()) { ok = false; break; }
                }
            }
            if !ok { continue; }

            // Bindings
            let mut scope: BTreeMap<String, Binding> = BTreeMap::new();
            let mut missing = None;
            for b in &ev.bindings {
                match payload.get(b) {
                    Some(v) => { scope.insert(b.clone(), Binding { value: v.clone(), ty: None }); }
                    None => { missing = Some(b.clone()); break; }
                }
            }
            if let Some(m) = missing {
                errors.push(format!("ERROR run event: line {}: event '{}' does not provide '{}'", ev.line, name, m));
                continue;
            }

            self.frames.push(Frame { scopes: alloc::vec![scope] });
            let r = self.exec_stmts(&ev.body, host);
            self.frames.pop();
            if let Err(e) = r {
                errors.push(format!("ERROR run event: {}", e));
            }
        }
        errors
    }

    /// Run a packaged handler (what the UI runtime does on click/hover).
    /// `args` are bound to the function's parameters; a block handler sees
    /// the first argument as `self`.
    pub fn run_handler(&mut self, handler: &Handler, args: Vec<VValue>, host: &mut dyn Host) -> Result<(), String> {
        self.steps = 0;
        match handler {
            Handler::Func(f) => {
                let f = f.clone();
                self.call_user(&f, args, host, true)?;
                Ok(())
            }
            Handler::Block(b) => {
                let mut scope: BTreeMap<String, Binding> = BTreeMap::new();
                if let Some(first) = args.into_iter().next() {
                    scope.insert("self".to_string(), Binding { value: first, ty: None });
                }
                if self.frames.len() >= MAX_CALL_DEPTH { return Err("call depth limit exceeded".to_string()); }
                self.frames.push(Frame { scopes: alloc::vec![scope] });
                let r = self.exec_stmts(b, host);
                self.frames.pop();
                r.map(|_| ())
            }
        }
    }

    /// Evaluate one expression with extra variables in scope (an element's
    /// named arguments and `self`). Used by the UI runtime for live properties.
    /// Gets its own step budget; never writes variables.
    pub fn eval_in(&mut self, expr: &Expr, scope: &BTreeMap<String, VValue>, host: &mut dyn Host) -> Result<VValue, String> {
        self.steps = 0;
        if self.frames.len() >= MAX_CALL_DEPTH {
            return Err("call depth limit exceeded".to_string());
        }
        let mut sc: BTreeMap<String, Binding> = BTreeMap::new();
        for (k, v) in scope {
            sc.insert(k.clone(), Binding { value: v.clone(), ty: None });
        }
        self.frames.push(Frame { scopes: alloc::vec![sc] });
        let r = self.eval(expr, host);
        self.frames.pop();
        r
    }

    /// Kernel-side write of a script-visible global (CPU_USAGE, ...).
    /// Creates the variable if missing; keeps a declared type if present.
    pub fn set_global(&mut self, name: &str, value: VValue) {
        if self.variables.get(name) == Some(&value) {
            return; // unchanged: no generation bump, no re-evaluation
        }
        self.generation = self.generation.wrapping_add(1);
        let v = match self.global_types.get(name) {
            Some(t) => t.coerce(value.clone()).unwrap_or(value),
            None => value,
        };
        self.variables.insert(name.to_string(), v);
    }

    pub fn get_global(&self, name: &str) -> Option<&VValue> {
        self.variables.get(name)
    }

    // ---------------- loading ----------------

    fn source_mut(&mut self, source: &str) -> &mut SourceInfo {
        self.sources.entry(source.to_string()).or_default()
    }

    fn run_program(&mut self, source: &str, program: &[Stmt], host: &mut dyn Host, errors: &mut Vec<String>) {
        self.source_mut(source);
        self.source_stack.push(source.to_string());

        // Hoist functions and event handlers so order in the file does not matter.
        for stmt in program {
            match &stmt.kind {
                StmtKind::Func(f) => {
                    if CORE_FUNCTIONS.contains(&f.name.as_str()) {
                        errors.push(format!("ERROR: line {}: '{}' is a built-in function and cannot be redefined", stmt.line, f.name));
                        continue;
                    }
                    if let Some((other, _)) = self.functions.get(&f.name) {
                        if other != source {
                            errors.push(format!("ERROR: line {}: function '{}' is already defined by {}", stmt.line, f.name, other));
                            continue;
                        }
                    }
                    self.functions.insert(f.name.clone(), (source.to_string(), f.clone()));
                    self.source_mut(source).functions.push(f.name.clone());
                }
                StmtKind::Event(e) => {
                    self.events.push((source.to_string(), e.clone()));
                    self.source_mut(source).event_count += 1;
                }
                _ => {}
            }
        }

        // Execute top-level statements in order. A top-level frame without
        // scopes means `var` at this level declares a global.
        self.frames.push(Frame { scopes: Vec::new() });
        for stmt in program {
            match &stmt.kind {
                StmtKind::Func(_) | StmtKind::Event(_) => continue,
                _ => {}
            }
            match self.exec_stmt(stmt, host) {
                Ok(Flow::Normal) => {}
                Ok(_) => errors.push(format!("ERROR: line {}: 'break/continue/return' outside of a loop or function", stmt.line)),
                Err(e) => errors.push(format!("ERROR: {}", e)),
            }
        }
        self.frames.pop();
        self.source_stack.pop();
    }

    fn do_import(&mut self, path: &str, host: &mut dyn Host, line: u32) -> Result<(), String> {
        if self.source_stack.len() >= MAX_IMPORT_DEPTH {
            return Err(format!("line {}: imports nested too deeply", line));
        }
        if self.source_stack.iter().any(|s| s == path) {
            return Err(format!("line {}: circular import of '{}'", line, path));
        }
        let parent = self.source_stack.last().cloned().unwrap_or_default();
        // Import-once: an already loaded source is not run again.
        if self.sources.contains_key(path) {
            let info = self.source_mut(&parent);
            if !info.imports.iter().any(|i| i == path) { info.imports.push(path.to_string()); }
            return Ok(());
        }
        let text = host.read_file(path).map_err(|e| format!("line {}: cannot import '{}': {}", line, path, e))?;
        let tokens = lex(&text).map_err(|e| format!("line {}: import '{}': LEX ERROR: {}", line, path, e))?;
        let program = parse_program(tokens).map_err(|e| format!("line {}: import '{}': PARSE ERROR: {}", line, path, e))?;
        self.source_mut(&parent).imports.push(path.to_string());
        let mut errs = Vec::new();
        // Run the imported program with a clean frame stack position: it
        // pushes its own top-level frame.
        self.run_program(path, &program, host, &mut errs);
        if let Some(first) = errs.into_iter().next() {
            return Err(format!("line {}: import '{}': {}", line, path, first));
        }
        Ok(())
    }

    // ---------------- scopes ----------------

    fn lookup(&self, name: &str) -> Option<&VValue> {
        if let Some(frame) = self.frames.last() {
            for scope in frame.scopes.iter().rev() {
                if let Some(b) = scope.get(name) { return Some(&b.value); }
            }
        }
        self.variables.get(name)
    }

    fn declare(&mut self, name: &str, value: VValue, ty: Option<Type>) {
        self.generation = self.generation.wrapping_add(1);
        if let Some(frame) = self.frames.last_mut() {
            if let Some(scope) = frame.scopes.last_mut() {
                scope.insert(name.to_string(), Binding { value, ty });
                return;
            }
        }
        // Global declaration.
        self.variables.insert(name.to_string(), value);
        match ty {
            Some(t) => { self.global_types.insert(name.to_string(), t); }
            None => { self.global_types.remove(name); }
        }
        if let Some(src) = self.source_stack.last().cloned() {
            let info = self.source_mut(&src);
            if !info.globals.iter().any(|g| g == name) { info.globals.push(name.to_string()); }
        }
    }

    /// Find the binding for `name`: local scopes first, then globals.
    /// Returns a mutable reference to the value and its declared type.
    fn binding_mut(&mut self, name: &str) -> Option<(&mut VValue, Option<Type>)> {
        let mut found_local = None;
        if let Some(frame) = self.frames.last() {
            for (i, scope) in frame.scopes.iter().enumerate().rev() {
                if scope.contains_key(name) { found_local = Some(i); break; }
            }
        }
        if let Some(i) = found_local {
            let frame = self.frames.last_mut().unwrap();
            let b = frame.scopes[i].get_mut(name).unwrap();
            let ty = b.ty.clone();
            return Some((&mut b.value, ty));
        }
        if self.variables.contains_key(name) {
            let ty = self.global_types.get(name).cloned();
            let v = self.variables.get_mut(name).unwrap();
            return Some((v, ty));
        }
        None
    }

    // ---------------- statements ----------------

    fn tick(&mut self, line: u32) -> Result<(), String> {
        self.steps += 1;
        if self.steps > MAX_STEPS {
            return Err(format!("line {}: step limit exceeded (possible runaway script)", line));
        }
        Ok(())
    }

    fn exec_stmts(&mut self, stmts: &[Stmt], host: &mut dyn Host) -> Result<Flow, String> {
        for s in stmts {
            match self.exec_stmt(s, host)? {
                Flow::Normal => {}
                other => return Ok(other),
            }
        }
        Ok(Flow::Normal)
    }

    /// Run statements inside a fresh block scope.
    fn exec_block(&mut self, stmts: &[Stmt], host: &mut dyn Host) -> Result<Flow, String> {
        if let Some(f) = self.frames.last_mut() { f.scopes.push(BTreeMap::new()); }
        let r = self.exec_stmts(stmts, host);
        if let Some(f) = self.frames.last_mut() { f.scopes.pop(); }
        r
    }

    fn exec_stmt(&mut self, stmt: &Stmt, host: &mut dyn Host) -> Result<Flow, String> {
        self.tick(stmt.line)?;
        let line = stmt.line;
        let wrap = |e: String| -> String {
            if e.starts_with("line ") { e } else { format!("line {}: {}", line, e) }
        };
        match &stmt.kind {
            StmtKind::VarDecl { name, ty, value } => {
                let built = self.build_var_value(name, ty, value, host).map_err(wrap);
                let v = match built {
                    Ok(v) => v,
                    Err(e) => {
                        self.failed_decls.insert(name.clone());
                        return Err(e);
                    }
                };
                self.failed_decls.remove(name);
                self.declare(name, v, ty.clone());
                Ok(Flow::Normal)
            }
            StmtKind::Assign { target, value } => {
                let v = self.eval(value, host).map_err(wrap)?;
                self.assign(target, v, host).map_err(wrap)?;
                Ok(Flow::Normal)
            }
            StmtKind::SetSystem { name, value } => {
                self.generation = self.generation.wrapping_add(1);
                let v = self.eval(value, host).map_err(wrap)?;
                host.set_system(name, &v).map_err(|m| wrap(format!("set {}: {}", name, m)))?;
                // The value is now readable from scripts (THEME[accent], ...).
                self.variables.insert(name.clone(), v);
                if let Some(src) = self.source_stack.last().cloned() {
                    let info = self.source_mut(&src);
                    if !info.globals.iter().any(|g| g == name) { info.globals.push(name.clone()); }
                }
                Ok(Flow::Normal)
            }
            StmtKind::If { cond, then_body, else_body } => {
                let c = self.eval(cond, host).map_err(wrap)?;
                if truthy(&c).map_err(wrap)? {
                    self.exec_block(then_body, host)
                } else if let Some(eb) = else_body {
                    self.exec_block(eb, host)
                } else {
                    Ok(Flow::Normal)
                }
            }
            StmtKind::For { var, iter, body } => {
                let it = self.eval(iter, host).map_err(wrap)?;
                // Iterate over a snapshot so the body may mutate the list.
                let items: Vec<VValue> = match it {
                    VValue::Array(a) => a,
                    VValue::Object(m) => m.keys().map(|k| VValue::String(k.clone())).collect(),
                    other => return Err(wrap(format!("cannot loop over {}", other.type_name()))),
                };
                for item in items {
                    self.tick(line)?;
                    if let Some(f) = self.frames.last_mut() {
                        let mut sc = BTreeMap::new();
                        sc.insert(var.clone(), Binding { value: item, ty: None });
                        f.scopes.push(sc);
                    }
                    let r = self.exec_stmts(body, host);
                    if let Some(f) = self.frames.last_mut() { f.scopes.pop(); }
                    match r? {
                        Flow::Normal | Flow::Continue => {}
                        Flow::Break => break,
                        Flow::Return(v) => return Ok(Flow::Return(v)),
                    }
                }
                Ok(Flow::Normal)
            }
            StmtKind::Return(e) => {
                let v = match e {
                    Some(e) => self.eval(e, host).map_err(wrap)?,
                    None => VValue::None,
                };
                Ok(Flow::Return(v))
            }
            StmtKind::Break => Ok(Flow::Break),
            StmtKind::Continue => Ok(Flow::Continue),
            StmtKind::Expr(e) => {
                self.eval(e, host).map_err(wrap)?;
                Ok(Flow::Normal)
            }
            StmtKind::Import(path) => {
                self.do_import(path, host, line)?;
                Ok(Flow::Normal)
            }
            StmtKind::Func(_) | StmtKind::Event(_) => {
                Err(wrap("'function' and 'on_event' are only allowed at the top level".to_string()))
            }
        }
    }

    /// Evaluate the initial value of a `var` declaration.
    fn build_var_value(
        &mut self,
        name: &str,
        ty: &Option<Type>,
        value: &Option<Expr>,
        host: &mut dyn Host,
    ) -> Result<VValue, String> {
        if let Some(Type::Element(Some(k))) = ty {
            if !host.is_element_kind(k) {
                return Err(format!("'{}': unknown element kind '{}'", name, k));
            }
        }
        match (ty, value) {
            // Lazy element definition: keep property expressions unevaluated.
            (Some(Type::Element(kind)), Some(Expr::Object(entries))) => {
                self.build_element_def(kind.clone(), entries, host)
            }
            (Some(t), Some(e)) => {
                let v = self.eval(e, host)?;
                t.coerce(v).map_err(|m| format!("'{}': {}", name, m))
            }
            (Some(t), None) => Ok(t.default_value()),
            (None, Some(e)) => self.eval(e, host),
            (None, None) => Ok(VValue::None),
        }
    }

    fn build_element_def(
        &mut self,
        kind: Option<String>,
        entries: &[ObjEntry],
        host: &mut dyn Host,
    ) -> Result<VValue, String> {
        let mut props: BTreeMap<String, Expr> = BTreeMap::new();
        for entry in entries {
            match entry {
                ObjEntry::Prop(k, e) => { props.insert(k.clone(), e.clone()); }
                ObjEntry::Spread(e) => {
                    match self.eval(e, host)? {
                        VValue::ElementDef(d) => {
                            for (k, v) in d.props.iter() { props.insert(k.clone(), v.clone()); }
                        }
                        VValue::Object(m) => {
                            for (k, v) in m { props.insert(k, Expr::Lit(v)); }
                        }
                        other => return Err(format!("cannot spread {} into an element", other.type_name())),
                    }
                }
            }
        }
        Ok(VValue::ElementDef(Arc::new(ElementDef { kind: kind.unwrap_or_default(), props })))
    }

    // ---------------- assignment ----------------

    fn assign(&mut self, target: &Target, value: VValue, host: &mut dyn Host) -> Result<(), String> {
        self.generation = self.generation.wrapping_add(1);
        // Evaluate path keys first (they may read variables).
        enum Key { Name(String), Idx(i64) }
        let mut keys: Vec<Key> = Vec::new();
        for seg in &target.path {
            match seg {
                PathSeg::Field(f) => keys.push(Key::Name(f.clone())),
                PathSeg::Index(e) => {
                    // Bare identifier: variable if defined, else a key name.
                    let k = match e {
                        Expr::Var(n) if self.lookup(n).is_none() => VValue::String(n.clone()),
                        other => self.eval(other, host)?,
                    };
                    match k {
                        VValue::Integer(i) | VValue::IndexVar(i) => keys.push(Key::Idx(i)),
                        VValue::String(s) => keys.push(Key::Name(s)),
                        other => return Err(format!("invalid key of type {}", other.type_name())),
                    }
                }
            }
        }

        let (slot, ty) = match self.binding_mut(&target.name) {
            Some(x) => x,
            None => return Err(format!(
                "assignment to undeclared variable '{}' (declare it with 'var {}')", target.name, target.name
            )),
        };

        if keys.is_empty() {
            let v = match &ty {
                Some(t) => t.coerce(value).map_err(|m| format!("'{}': {}", target.name, m))?,
                None => {
                    // Legacy: assigning a number to an `index` variable keeps it an index.
                    if let VValue::IndexVar(_) = slot {
                        match value { VValue::Integer(i) | VValue::IndexVar(i) => VValue::IndexVar(i), other => other }
                    } else { value }
                }
            };
            *slot = v;
            return Ok(());
        }

        // Walk into nested containers.
        let mut cur: &mut VValue = slot;
        let last = keys.len() - 1;
        for (i, k) in keys.iter().enumerate() {
            let is_last = i == last;
            match (cur, k) {
                (VValue::Object(map), Key::Name(n)) => {
                    if is_last {
                        map.insert(n.clone(), value);
                        return Ok(());
                    }
                    cur = match map.get_mut(n) {
                        Some(v) => v,
                        None => return Err(format!("no property '{}'", n)),
                    };
                }
                (VValue::Array(arr), Key::Idx(ix)) => {
                    let len = arr.len() as i64;
                    let real = if *ix < 0 { len + *ix } else { *ix };
                    if real < 0 || real >= len {
                        return Err(format!("list index {} out of range (length {})", ix, len));
                    }
                    if is_last {
                        arr[real as usize] = value;
                        return Ok(());
                    }
                    cur = &mut arr[real as usize];
                }
                (other, _) => {
                    return Err(format!("cannot assign into {}", other.type_name()));
                }
            }
        }
        Ok(())
    }

    // ---------------- expressions ----------------

    fn eval(&mut self, expr: &Expr, host: &mut dyn Host) -> Result<VValue, String> {
        match expr {
            Expr::Lit(v) => Ok(v.clone()),
            Expr::Var(name) => self.eval_var(name),
            Expr::Object(entries) => {
                let mut obj = BTreeMap::new();
                for entry in entries {
                    match entry {
                        ObjEntry::Prop(k, e) => {
                            let v = self.eval(e, host)?;
                            obj.insert(k.clone(), v);
                        }
                        ObjEntry::Spread(e) => match self.eval(e, host)? {
                            VValue::Object(m) => { for (k, v) in m { obj.insert(k, v); } }
                            other => return Err(format!("cannot spread {} into an object", other.type_name())),
                        },
                    }
                }
                Ok(VValue::Object(obj))
            }
            Expr::Array(items) => {
                let mut v = Vec::with_capacity(items.len());
                for e in items { v.push(self.eval(e, host)?); }
                Ok(VValue::Array(v))
            }
            Expr::Interp(parts) => {
                let mut s = String::new();
                for p in parts {
                    match p {
                        InterpPart::Lit(l) => s.push_str(l),
                        InterpPart::Expr(e) => s.push_str(&self.eval(e, host)?.to_display()),
                    }
                }
                Ok(VValue::String(s))
            }
            Expr::Concat(list) => {
                let mut s = String::new();
                for e in list { s.push_str(&self.eval(e, host)?.to_display()); }
                Ok(VValue::String(s))
            }
            Expr::Format(e) => Ok(VValue::String(self.eval(e, host)?.to_display())),
            Expr::Unary(op, e) => {
                let v = self.eval(e, host)?;
                match op {
                    UnOp::Not => Ok(VValue::Boolean(!truthy(&v)?)),
                    UnOp::Neg => match v {
                        VValue::Integer(i) => i.checked_neg().map(VValue::Integer).ok_or_else(|| "integer overflow".to_string()),
                        VValue::IndexVar(i) => i.checked_neg().map(VValue::IndexVar).ok_or_else(|| "integer overflow".to_string()),
                        VValue::Float(f) => Ok(VValue::Float(-f)),
                        other => Err(format!("cannot negate {}", other.type_name())),
                    },
                }
            }
            Expr::Binary(op, l, r) => self.eval_binary(*op, l, r, host),
            Expr::IfExpr(c, a, b) => {
                let cv = self.eval(c, host)?;
                if truthy(&cv)? { self.eval(a, host) } else { self.eval(b, host) }
            }
            Expr::Index(target, key) => {
                let t = self.eval(target, host)?;
                self.index_value(t, key, host)
            }
            Expr::Field(target, name) => {
                let t = self.eval(target, host)?;
                self.field_value(t, name, host)
            }
            Expr::Call(name, args) => self.eval_call(name, args, host),
            Expr::Method(target, name, args) => self.eval_method(target, name, args, host),
            Expr::AddElement { kind, args } => self.eval_add(kind, args, host),
            Expr::Block(b) => Ok(VValue::Handler(Handler::Block(b.clone()))),
        }
    }

    fn eval_var(&self, name: &str) -> Result<VValue, String> {
        if let Some(v) = self.lookup(name) { return Ok(v.clone()); }
        if let Some((_, f)) = self.functions.get(name) {
            return Ok(VValue::Handler(Handler::Func(f.clone())));
        }
        match name {
            "TRANSPARENT" => Ok(VValue::Color(Rgba::TRANSPARENT)),
            // Type names are values so `format(int, x)` reads naturally.
            "int" | "float" | "string" | "bool" => Ok(VValue::String(name.to_string())),
            _ if self.failed_decls.contains(name) => Err(format!(
                "undefined variable '{}' (its declaration failed earlier - fix the first error)", name
            )),
            _ => Err(format!("undefined variable '{}'", name)),
        }
    }

    fn field_value(&mut self, t: VValue, name: &str, host: &mut dyn Host) -> Result<VValue, String> {
        match t {
            VValue::Object(map) => Ok(map.get(name).cloned().unwrap_or(VValue::None)),
            VValue::Ref(id) => host.element_prop(id, name),
            VValue::ElementDef(d) => match d.props.get(name) {
                Some(Expr::Lit(v)) => Ok(v.clone()),
                Some(_) => Err(format!("property '{}' of an element definition is an unevaluated expression", name)),
                None => Ok(VValue::None),
            },
            other => Err(format!("cannot read property '{}' of {}", name, other.type_name())),
        }
    }

    fn index_value(&mut self, t: VValue, key: &Expr, host: &mut dyn Host) -> Result<VValue, String> {
        // A bare identifier key that is not a variable means the key NAME.
        let name_container = matches!(t, VValue::Object(_) | VValue::Ref(_));
        let k = match key {
            Expr::Var(n) if name_container && self.lookup(n).is_none() => VValue::String(n.clone()),
            other => self.eval(other, host)?,
        };
        match t {
            VValue::Object(map) => {
                let key = match &k {
                    VValue::String(s) => s.clone(),
                    VValue::Integer(i) | VValue::IndexVar(i) => format!("{}", i),
                    other => return Err(format!("invalid object key of type {}", other.type_name())),
                };
                Ok(map.get(&key).cloned().unwrap_or(VValue::None))
            }
            VValue::Ref(id) => match k {
                VValue::String(s) => host.element_prop(id, &s),
                other => Err(format!("element property name must be a string, got {}", other.type_name())),
            },
            VValue::Array(arr) => {
                let ix = int_of(&k).ok_or_else(|| format!("list index must be an int, got {}", k.type_name()))?;
                let len = arr.len() as i64;
                let real = if ix < 0 { len + ix } else { ix };
                if real < 0 || real >= len {
                    return Err(format!("list index {} out of range (length {})", ix, len));
                }
                Ok(arr[real as usize].clone())
            }
            VValue::String(s) => {
                let mut num = int_of(&k).ok_or_else(|| "string index must be an int".to_string())?;
                let chars: Vec<char> = s.chars().collect();
                if num < 0 { num += chars.len() as i64; }
                if num >= 0 && (num as usize) < chars.len() {
                    Ok(VValue::String(chars[num as usize].to_string()))
                } else {
                    Ok(VValue::String("?".to_string()))
                }
            }
            other => Err(format!("cannot index {}", other.type_name())),
        }
    }

    fn eval_binary(&mut self, op: BinOp, l: &Expr, r: &Expr, host: &mut dyn Host) -> Result<VValue, String> {
        // Short-circuit logic first.
        if op == BinOp::And {
            let lv = self.eval(l, host)?;
            if !truthy(&lv)? { return Ok(VValue::Boolean(false)); }
            let rv = self.eval(r, host)?;
            return Ok(VValue::Boolean(truthy(&rv)?));
        }
        if op == BinOp::Or {
            let lv = self.eval(l, host)?;
            if truthy(&lv)? { return Ok(VValue::Boolean(true)); }
            let rv = self.eval(r, host)?;
            return Ok(VValue::Boolean(truthy(&rv)?));
        }
        let lv = self.eval(l, host)?;
        let rv = self.eval(r, host)?;
        match op {
            BinOp::Eq => Ok(VValue::Boolean(values_equal(&lv, &rv))),
            BinOp::Ne => Ok(VValue::Boolean(!values_equal(&lv, &rv))),
            BinOp::Add => self.add_values(lv, rv),
            BinOp::Sub | BinOp::Mul | BinOp::Div | BinOp::Rem => self.arith(op, &lv, &rv),
            BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge => {
                let ord = match (&lv, &rv) {
                    (VValue::String(a), VValue::String(b)) => Some(a.cmp(b)),
                    _ => match (int_of(&lv), int_of(&rv)) {
                        (Some(a), Some(b)) => Some(a.cmp(&b)),
                        _ => match (numeric(&lv), numeric(&rv)) {
                            (Some(a), Some(b)) => a.partial_cmp(&b),
                            _ => return Err(format!("cannot compare {} with {}", lv.type_name(), rv.type_name())),
                        },
                    },
                };
                let ord = ord.ok_or_else(|| "cannot compare these numbers".to_string())?;
                use core::cmp::Ordering::*;
                let res = match op {
                    BinOp::Lt => ord == Less,
                    BinOp::Le => ord != Greater,
                    BinOp::Gt => ord == Greater,
                    _ => ord != Less,
                };
                Ok(VValue::Boolean(res))
            }
            BinOp::And | BinOp::Or => unreachable!(),
        }
    }

    fn add_values(&self, lv: VValue, rv: VValue) -> Result<VValue, String> {
        match (&lv, &rv) {
            // Legacy: string on either side concatenates.
            (VValue::String(a), b) => Ok(VValue::String(format!("{}{}", a, b.to_display()))),
            (a, VValue::String(b)) => Ok(VValue::String(format!("{}{}", a.to_display(), b))),
            (VValue::IndexVar(a), VValue::Integer(b)) | (VValue::IndexVar(a), VValue::IndexVar(b)) => {
                a.checked_add(*b).map(VValue::IndexVar).ok_or_else(|| "integer overflow".to_string())
            }
            (VValue::Array(a), VValue::Array(b)) => {
                let mut out = a.clone();
                out.extend(b.iter().cloned());
                Ok(VValue::Array(out))
            }
            _ => self.arith(BinOp::Add, &lv, &rv),
        }
    }

    fn arith(&self, op: BinOp, lv: &VValue, rv: &VValue) -> Result<VValue, String> {
        let sym = match op { BinOp::Add => "+", BinOp::Sub => "-", BinOp::Mul => "*", BinOp::Div => "/", _ => "%" };
        if let (Some(a), Some(b)) = (int_of(lv), int_of(rv)) {
            let r = match op {
                BinOp::Add => a.checked_add(b),
                BinOp::Sub => a.checked_sub(b),
                BinOp::Mul => a.checked_mul(b),
                BinOp::Div => {
                    if b == 0 { return Err("division by zero".to_string()); }
                    a.checked_div(b)
                }
                _ => {
                    if b == 0 { return Err("modulo by zero".to_string()); }
                    a.checked_rem(b)
                }
            };
            return r.map(VValue::Integer).ok_or_else(|| "integer overflow".to_string());
        }
        if let (Some(a), Some(b)) = (numeric(lv), numeric(rv)) {
            let r = match op {
                BinOp::Add => a + b,
                BinOp::Sub => a - b,
                BinOp::Mul => a * b,
                BinOp::Div => {
                    if b == 0.0 { return Err("division by zero".to_string()); }
                    a / b
                }
                _ => {
                    if b == 0.0 { return Err("modulo by zero".to_string()); }
                    let q = a / b;
                    if q >= 9.0e15 || q <= -9.0e15 { return Err("float modulo out of range".to_string()); }
                    a - ((q as i64) as f64) * b
                }
            };
            return Ok(VValue::Float(r));
        }
        Err(format!("cannot apply '{}' to {} and {}", sym, lv.type_name(), rv.type_name()))
    }

    // ---------------- calls ----------------

    fn eval_args(&mut self, args: &[Arg], host: &mut dyn Host) -> Result<Vec<CallArg>, String> {
        let mut out = Vec::with_capacity(args.len());
        for a in args {
            out.push(CallArg { name: a.name.clone(), value: self.eval(&a.value, host)? });
        }
        Ok(out)
    }

    fn eval_call(&mut self, name: &str, args: &[Arg], host: &mut dyn Host) -> Result<VValue, String> {
        self.tick(0)?;
        let evaluated = self.eval_args(args, host)?;

        if let Some((_, f)) = self.functions.get(name).cloned() {
            if evaluated.iter().any(|a| a.name.is_some()) {
                return Err(format!("function '{}' does not take named arguments", name));
            }
            let vals: Vec<VValue> = evaluated.into_iter().map(|a| a.value).collect();
            if vals.len() != f.params.len() {
                return Err(format!("function '{}' takes {} argument(s), got {}", name, f.params.len(), vals.len()));
            }
            return self.call_user(&f, vals, host, false);
        }

        match name {
            "format" => return self.builtin_format(&evaluated),
            "len" => {
                if evaluated.len() != 1 { return Err("len() takes exactly 1 argument".to_string()); }
                return match &evaluated[0].value {
                    VValue::Array(a) => Ok(VValue::Integer(a.len() as i64)),
                    VValue::String(s) => Ok(VValue::Integer(s.chars().count() as i64)),
                    VValue::Object(m) => Ok(VValue::Integer(m.len() as i64)),
                    other => Err(format!("len() does not work on {}", other.type_name())),
                };
            }
            _ => {}
        }

        match host.call(name, &evaluated) {
            Some(r) => r,
            None => Err(format!("unknown function '{}'", name)),
        }
    }

    fn call_user(&mut self, f: &Arc<FuncDef>, args: Vec<VValue>, host: &mut dyn Host, lenient_args: bool) -> Result<VValue, String> {
        if self.frames.len() >= MAX_CALL_DEPTH {
            return Err("call depth limit exceeded".to_string());
        }
        let mut scope: BTreeMap<String, Binding> = BTreeMap::new();
        let mut it = args.into_iter();
        for p in &f.params {
            match it.next() {
                Some(v) => { scope.insert(p.clone(), Binding { value: v, ty: None }); }
                None => {
                    return Err(format!("function '{}' is missing argument '{}'", f.name, p));
                }
            }
        }
        // Extra arguments are only tolerated for runtime-invoked handlers
        // (the runtime always offers `self`).
        if !lenient_args && it.next().is_some() {
            return Err(format!("function '{}' got too many arguments", f.name));
        }
        self.frames.push(Frame { scopes: alloc::vec![scope] });
        let r = self.exec_stmts(&f.body, host);
        self.frames.pop();
        match r? {
            Flow::Return(v) => Ok(v),
            Flow::Normal => Ok(VValue::None),
            Flow::Break | Flow::Continue => Err(format!("'break/continue' outside of a loop in '{}'", f.name)),
        }
    }

    fn builtin_format(&self, args: &[CallArg]) -> Result<VValue, String> {
        if args.len() < 2 || args.len() > 3 {
            return Err("format(type, value[, decimals]) takes 2 or 3 arguments".to_string());
        }
        let ty = match &args[0].value {
            VValue::String(s) => s.as_str(),
            _ => return Err("format(): first argument must be int, float, string or bool".to_string()),
        };
        let v = &args[1].value;
        match ty {
            "int" => match v {
                VValue::Integer(i) | VValue::IndexVar(i) => Ok(VValue::String(format!("{}", i))),
                VValue::Float(f) => Ok(VValue::String(format!("{}", *f as i64))),
                other => Err(format!("format(int, ...) cannot format {}", other.type_name())),
            },
            "float" => {
                let decimals = match args.get(2) {
                    Some(a) => match int_of(&a.value) {
                        Some(d) if (0..=9).contains(&d) => d as usize,
                        _ => return Err("format(float, value, decimals): decimals must be 0..9".to_string()),
                    },
                    None => 2,
                };
                match numeric(v) {
                    Some(f) => Ok(VValue::String(format!("{:.*}", decimals, f))),
                    None => Err(format!("format(float, ...) cannot format {}", v.type_name())),
                }
            }
            "string" => Ok(VValue::String(v.to_display())),
            "bool" => match v {
                VValue::Boolean(b) => Ok(VValue::String(if *b { "true".to_string() } else { "false".to_string() })),
                other => Err(format!("format(bool, ...) cannot format {}", other.type_name())),
            },
            other => Err(format!("format(): unknown type '{}' (use int, float, string or bool)", other)),
        }
    }

    fn eval_add(&mut self, kind: &str, args: &[Arg], host: &mut dyn Host) -> Result<VValue, String> {
        self.tick(0)?;
        if !host.is_element_kind(kind) {
            return Err(format!("unknown element kind '{}'", kind));
        }
        if args.is_empty() || args[0].name.is_some() {
            return Err(format!("add element.{}(...) needs a definition as its first argument", kind));
        }
        let def = match self.eval(&args[0].value, host)? {
            VValue::ElementDef(d) => d,
            other => return Err(format!("add element.{}(...): first argument must be an element definition, got {}", kind, other.type_name())),
        };
        if !def.kind.is_empty() && def.kind != kind {
            return Err(format!("definition is element.{} but was added as element.{}", def.kind, kind));
        }
        let mut named = Vec::new();
        for a in &args[1..] {
            match &a.name {
                Some(n) => named.push(CallArg { name: Some(n.clone()), value: self.eval(&a.value, host)? }),
                None => return Err("add element.<kind>(...): arguments after the definition must be named (name: value)".to_string()),
            }
        }
        // Snapshot properties are baked NOW, in the element's scope (its named
        // arguments are variables there). A named argument with the same key
        // replaces the definition's property, so it is not evaluated.
        let mut scope: BTreeMap<String, VValue> = BTreeMap::new();
        for a in &named {
            if let Some(n) = &a.name {
                scope.insert(n.clone(), a.value.clone());
            }
        }
        let mut snapshot: Vec<(String, Result<VValue, String>)> = Vec::new();
        for (k, e) in def.props.iter() {
            if scope.contains_key(k) {
                continue;
            }
            if k.starts_with("on_") || host.snapshot_key(k) {
                let saved = self.steps;
                let r = self.eval_in(e, &scope, host);
                self.steps = saved;
                snapshot.push((k.clone(), r));
            }
        }
        host.add_element(kind, &def, &named, &snapshot)
    }

    fn eval_method(&mut self, target: &Expr, name: &str, args: &[Arg], host: &mut dyn Host) -> Result<VValue, String> {
        self.tick(0)?;
        let evaluated = self.eval_args(args, host)?;
        if evaluated.iter().any(|a| a.name.is_some()) {
            return Err(format!("method '{}' does not take named arguments", name));
        }
        let vals: Vec<VValue> = evaluated.into_iter().map(|a| a.value).collect();

        let mutating = matches!(name, "append" | "remove" | "clear");
        if mutating {
            let t = expr_to_target(target)
                .ok_or_else(|| format!("'{}' needs a variable on the left, e.g. my_list.{}(...)", name, name))?;
            return self.mutate_method(&t, name, vals, host);
        }

        let recv = self.eval(target, host)?;
        match (&recv, name) {
            (VValue::Array(a), "len") => Ok(VValue::Integer(a.len() as i64)),
            (VValue::Array(a), "contains") => {
                let x = vals.get(0).ok_or_else(|| "contains() takes 1 argument".to_string())?;
                Ok(VValue::Boolean(a.iter().any(|e| values_equal(e, x))))
            }
            (VValue::String(s), "len") => Ok(VValue::Integer(s.chars().count() as i64)),
            (VValue::Object(m), "len") => Ok(VValue::Integer(m.len() as i64)),
            (VValue::Object(m), "has") => {
                match vals.get(0) {
                    Some(VValue::String(k)) => Ok(VValue::Boolean(m.contains_key(k))),
                    _ => Err("has() takes a string key".to_string()),
                }
            }
            (other, m) => Err(format!("{} has no method '{}'", other.type_name(), m)),
        }
    }

    fn mutate_method(&mut self, target: &Target, name: &str, vals: Vec<VValue>, host: &mut dyn Host) -> Result<VValue, String> {
        // Resolve the list by reading it, mutating a copy, and writing it back
        // through the normal assignment path (keeps type checks and nesting).
        let current = {
            let mut e = Expr::Var(target.name.clone());
            for seg in &target.path {
                e = match seg {
                    PathSeg::Field(f) => Expr::Field(Box::new(e), f.clone()),
                    PathSeg::Index(k) => Expr::Index(Box::new(e), Box::new(k.clone())),
                };
            }
            self.eval(&e, host)?
        };
        let mut list = match current {
            VValue::Array(a) => a,
            other => return Err(format!("'{}' is {} - only lists have '{}'", target.name, other.type_name(), name)),
        };
        let result = match name {
            "append" => {
                if vals.len() != 1 { return Err("append() takes exactly 1 argument".to_string()); }
                list.push(vals.into_iter().next().unwrap());
                VValue::None
            }
            "remove" => {
                if vals.len() != 1 { return Err("remove() takes exactly 1 argument".to_string()); }
                let pos = list.iter().position(|e| values_equal(e, &vals[0]));
                match pos {
                    Some(p) => { list.remove(p); VValue::Boolean(true) }
                    None => VValue::Boolean(false),
                }
            }
            _ => {
                if !vals.is_empty() { return Err("clear() takes no arguments".to_string()); }
                list.clear();
                VValue::None
            }
        };
        self.assign(target, VValue::Array(list), host)?;
        Ok(result)
    }
}

// Silence "unused" warnings for items only used by some build configurations.
#[allow(dead_code)]
fn _unused(_: &BTreeSet<String>, _: &Box<u8>) {}
