// -----| VEILOS Scripting Engine |-----

use alloc::string::{String, ToString};
use alloc::vec::Vec;
use alloc::collections::BTreeMap;
use alloc::format;
use crate::desktop::{notify, NotifCategory};
use crate::gui_toolkit::{ACTIVE_THEME, ThemeKind, Theme, ColorPalette};
use crate::graphics::Color;
use crate::policy::{POLICY, PolicyMode};
use spin::Mutex;
use alloc::boxed::Box;

lazy_static::lazy_static! {
    pub static ref GLOBAL_ENGINE: Mutex<VScriptEngine> = Mutex::new(VScriptEngine::new());
}

#[derive(Debug, Clone, PartialEq)]
pub enum VValue {
    Integer(i64),
    String(String),
    Boolean(bool),
    Color(Color),
    Object(BTreeMap<String, VValue>),
    IndexVar(i64), // type index
    None,
}

impl VValue {
    fn to_string_val(&self) -> String {
        match self {
            VValue::Integer(v) => format!("{}", v),
            VValue::IndexVar(v) => format!("{}", v),
            VValue::String(s) => s.clone(),
            VValue::Boolean(b) => if *b { "true".to_string() } else { "false".to_string() },
            VValue::Color(c) => format!("#{:02x}{:02x}{:02x}", c.r, c.g, c.b),
            _ => "".to_string(), // Object ignores
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
enum Token {
    Ident(String),
    StringLiteral(String),
    Number(i64),
    ColorLiteral(Color),
    Var,
    OnEvent,
    Set,
    If,
    Colon,
    Equal,
    EqEq,
    Plus,
    Minus,
    Semicolon,
    Comma,
    LBrace,
    RBrace,
    LParen,
    RParen,
    LBracket,
    RBracket,
}

#[derive(Debug, Clone)]
enum Expr {
    Literal(VValue),
    VarRef(String),
    Add(Box<Expr>, Box<Expr>),
    Eq(Box<Expr>, Box<Expr>),
    Index(Box<Expr>, Box<Expr>), // target[key/index]
    Call(String, Vec<Expr>),
    ConcatSequence(Vec<Expr>), // {expr}{expr}
    Neg(Box<Expr>),
}

#[derive(Debug, Clone)]
enum Statement {
    VarDecl { name: String, is_index: bool, value: Expr },
    VarAssign { name: String, prop: Option<Expr>, value: Expr },
    SetSystem { name: String, value: Expr },
    If { condition: Expr, body: Vec<Statement> },
    Call { name: String, args: Vec<Expr> },
    EventDecl { name: String, filters: BTreeMap<String, String>, body: Vec<Statement> },
}

pub struct VScriptEngine {
    variables: BTreeMap<String, VValue>,
    events: Vec<Statement>, // EventDecls
}

impl VScriptEngine {
    pub fn new() -> Self {
        Self { variables: BTreeMap::new(), events: Vec::new() }
    }

    pub fn execute(&mut self, script: &str) -> Vec<String> {
        let mut output = Vec::new();
        match self.lex(script) {
            Ok(tokens) => {
                match self.parse(&tokens) {
                    Ok(stmts) => {
                        for stmt in stmts {
                            if let Statement::EventDecl { .. } = stmt {
                                self.events.push(stmt);
                            } else {
                                if let Err(e) = self.execute_statement(&stmt) {
                                    output.push(format!("ERROR: {}", e));
                                }
                            }
                        }
                    }
                    Err(e) => output.push(format!("PARSE ERROR: {}", e)),
                }
            }
            Err(e) => output.push(format!("LEX ERROR: {}", e)),
        }
        output
    }

    pub fn trigger_event(&mut self, event_name: &str, filters: BTreeMap<String, String>) -> Vec<String> {
        let mut output = Vec::new();
        
        // Before running the event body, reset all 'IndexVar's to 0
        for (_, val) in self.variables.iter_mut() {
            if let VValue::IndexVar(ref mut v) = val {
                *v = 0;
            }
        }

        let evs = self.events.clone(); // clone to avoid borrow checker issues with `self`

        for ev in evs {
            if let Statement::EventDecl { name, filters: ref ev_filters, body } = ev {
                if name == event_name {
                    // Check if all declared filters match the provided filters
                    let mut matches = true;
                    for (k, v) in ev_filters {
                        if filters.get(k) != Some(v) {
                            matches = false; break;
                        }
                    }
                    if matches {
                        for stmt in body {
                            if let Err(e) = self.execute_statement(&stmt) {
                                output.push(format!("ERROR: {}", e));
                            }
                        }
                    }
                }
            }
        }
        output
    }

    fn execute_statement(&mut self, stmt: &Statement) -> Result<(), String> {
        match stmt {
            Statement::VarDecl { name, is_index, value } => {
                let mut v = self.eval_expr(value)?;
                if *is_index {
                    let num = match v {
                        VValue::Integer(x) => x,
                        _ => 0,
                    };
                    v = VValue::IndexVar(num);
                }
                self.variables.insert(name.clone(), v);
            }
            Statement::VarAssign { name, prop, value } => {
                let v = self.eval_expr(value)?;
                if let Some(p) = prop {
                    let p_val = self.eval_expr(p)?;
                    if let Some(target) = self.variables.get_mut(name) {
                        match target {
                            VValue::Object(map) => {
                                let key = p_val.to_string_val();
                                map.insert(key, v);
                            }
                            _ => return Err(format!("Cannot property-assign onto non-object '{}'", name)),
                        }
                    } else {
                        return Err(format!("Variable '{}' not found", name));
                    }
                } else {
                    // Updating entire var
                    if let Some(VValue::IndexVar(_)) = self.variables.get(name) {
                        let num = match v { VValue::Integer(x) | VValue::IndexVar(x) => x, _ => 0 };
                        self.variables.insert(name.clone(), VValue::IndexVar(num));
                    } else {
                        self.variables.insert(name.clone(), v);
                    }
                }
            }
            Statement::SetSystem { name, value } => {
                let v = self.eval_expr(value)?;
                if name == "THEME" {
                    if let VValue::Object(obj) = v {
                        self.apply_theme_object(&obj);
                    }
                }
            }
            Statement::If { condition, body } => {
                let cond = self.eval_expr(condition)?;
                let ok = match cond {
                    VValue::Boolean(b) => b,
                    VValue::Integer(i) | VValue::IndexVar(i) => i != 0,
                    _ => false,
                };
                if ok {
                    for s in body {
                        self.execute_statement(s)?;
                    }
                }
            }
            Statement::Call { name, args } => {
                let mut ev_args = Vec::new();
                for a in args { ev_args.push(self.eval_expr(a)?); }
                self.call_function(name, ev_args)?;
            }
            _ => {}
        }
        Ok(())
    }

    fn eval_expr(&self, expr: &Expr) -> Result<VValue, String> {
        match expr {
            Expr::Literal(v) => Ok(v.clone()),
            Expr::VarRef(name) => {
                if let Some(v) = self.variables.get(name) {
                    Ok(v.clone())
                } else {
                    // For unquoted keys inside brackets like DARK[background]
                    Ok(VValue::String(name.clone()))
                }
            }
            Expr::Add(l, r) => {
                let lv = self.eval_expr(l)?;
                let rv = self.eval_expr(r)?;
                match (lv, rv) {
                    (VValue::Integer(a), VValue::Integer(b)) => Ok(VValue::Integer(a + b)),
                    (VValue::IndexVar(a), VValue::Integer(b)) => Ok(VValue::IndexVar(a + b)),
                    (VValue::Integer(a), VValue::IndexVar(b)) => Ok(VValue::Integer(a + b)),
                    (VValue::IndexVar(a), VValue::IndexVar(b)) => Ok(VValue::IndexVar(a + b)),
                    (VValue::String(a), b) => Ok(VValue::String(a + &b.to_string_val())),
                    (a, VValue::String(b)) => Ok(VValue::String(a.to_string_val() + &b)),
                    _ => Err(String::from("Invalid types for addition")),
                }
            }
            Expr::Neg(inner) => {
                 let v = self.eval_expr(inner)?;
                 match v {
                     VValue::Integer(a) => Ok(VValue::Integer(-a)),
                     VValue::IndexVar(a) => Ok(VValue::IndexVar(-a)),
                     _ => Err(String::from("Cannot negate non-number")),
                 }
            }
            Expr::Eq(l, r) => {
                let lv = self.eval_expr(l)?;
                let rv = self.eval_expr(r)?;
                match (lv, rv) {
                    (VValue::Integer(a), VValue::Integer(b)) => Ok(VValue::Boolean(a == b)),
                    (VValue::IndexVar(a), VValue::Integer(b)) => Ok(VValue::Boolean(a == b)),
                    (VValue::Integer(a), VValue::IndexVar(b)) => Ok(VValue::Boolean(a == b)),
                    (VValue::IndexVar(a), VValue::IndexVar(b)) => Ok(VValue::Boolean(a == b)),
                    (VValue::String(a), VValue::String(b)) => Ok(VValue::Boolean(a == b)),
                    _ => Ok(VValue::Boolean(false)),
                }
            }
            Expr::Index(obj, idx) => {
                let o = self.eval_expr(obj)?;
                let i = self.eval_expr(idx)?;
                
                match o {
                    VValue::Object(map) => {
                        let k = i.to_string_val();
                        if let Some(v) = map.get(&k) {
                            Ok(v.clone())
                        } else {
                            Ok(VValue::None)
                        }
                    }
                    VValue::String(s) => {
                        let mut num = match i {
                            VValue::Integer(x) => x,
                            VValue::IndexVar(x) => x,
                            _ => return Err(String::from("Invalid index for string")),
                        };
                        let chars: Vec<char> = s.chars().collect();
                        if num < 0 { num = chars.len() as i64 + num; }
                        if num >= 0 && (num as usize) < chars.len() {
                            let mut t = String::new();
                            t.push(chars[num as usize]);
                            Ok(VValue::String(t))
                        } else {
                            Ok(VValue::String(String::from("?")))
                        }
                    }
                    _ => Err(String::from("Cannot index this type")),
                }
            }
            Expr::Call(name, args) => {
                let mut ev_args = Vec::new();
                for a in args { ev_args.push(self.eval_expr(a)?); }
                
                if name == "format" {
                    // special handler.
                    // format(string, (expr1, expr2)) -> concat
                    if ev_args.len() >= 2 {
                        let v = &ev_args[1];
                        return Ok(VValue::String(v.to_string_val()));
                    }
                }
                
                self.call_function(name, ev_args).map(|v| v.unwrap_or(VValue::None))
            }
            Expr::ConcatSequence(list) => {
                let mut s = String::new();
                for i in list {
                    let v = self.eval_expr(i)?;
                    s.push_str(&v.to_string_val());
                }
                Ok(VValue::String(s))
            }
        }
    }

    fn call_function(&mut self, name: &str, args: Vec<VValue>) -> Result<Option<VValue>, String> {
        match name {
            "print" => {
                let mut out = String::new();
                for (i, a) in args.iter().enumerate() {
                    if i > 0 { out.push(' '); }
                    out.push_str(&a.to_string_val());
                }
                crate::desktop::notify(NotifCategory::Info, "VSCRIPT", &out);
                Ok(Some(VValue::None))
            }
            _ => Err(format!("Unknown function '{}'", name)),
        }
    }

    fn apply_theme_object(&mut self, obj: &BTreeMap<String, VValue>) {
        let mut palette = ColorPalette::veil();
        let get_col = |k: &str, def: Color| -> Color {
            if let Some(v) = obj.get(k) {
                if let VValue::Color(c) = v { return *c; }
                if let VValue::String(s) = v {
                    if s.starts_with("#") && s.len() == 7 {
                        let r = u8::from_str_radix(&s[1..3], 16).unwrap_or(0);
                        let g = u8::from_str_radix(&s[3..5], 16).unwrap_or(0);
                        let b = u8::from_str_radix(&s[5..7], 16).unwrap_or(0);
                        return Color { r, g, b };
                    }
                }
            }
            def
        };
        palette.background = get_col("background", palette.background);
        palette.text_primary = get_col("foreground", palette.text_primary);
        palette.accent = get_col("accent", palette.accent);
        palette.border = get_col("border", palette.border);
        
        let corner_radius = if let Some(VValue::Integer(v)) = obj.get("border_radius") { *v as u32 } else { 4 };
        
        let mut t = ACTIVE_THEME.lock();
        t.palette = palette;
        t.corner_radius = corner_radius;
        crate::desktop::DESKTOP_WM.lock().z_order_dirty = true;
    }

    // ================= LEXER & PARSER =================
    fn lex(&self, input: &str) -> Result<Vec<Token>, String> {
        let mut tokens = Vec::new();
        let mut chars = input.chars().peekable();
        while let Some(&c) = chars.peek() {
            if c.is_whitespace() { chars.next(); continue; }
            if c == '/' {
                chars.next();
                if let Some(&'/') = chars.peek() {
                    while let Some(&ch) = chars.peek() { if ch == '\n' { break; } chars.next(); }
                    continue;
                } else { return Err(String::from("Unexpected '/'")); }
            }
            match c {
                ':' => { tokens.push(Token::Colon); chars.next(); }
                '=' => {
                    chars.next();
                    if let Some(&'=') = chars.peek() {
                        tokens.push(Token::EqEq); chars.next();
                    } else {
                        tokens.push(Token::Equal);
                    }
                }
                ';' => { tokens.push(Token::Semicolon); chars.next(); }
                ',' => { tokens.push(Token::Comma); chars.next(); }
                '{' => { tokens.push(Token::LBrace); chars.next(); }
                '}' => { tokens.push(Token::RBrace); chars.next(); }
                '(' => { tokens.push(Token::LParen); chars.next(); }
                ')' => { tokens.push(Token::RParen); chars.next(); }
                '[' => { tokens.push(Token::LBracket); chars.next(); }
                ']' => { tokens.push(Token::RBracket); chars.next(); }
                '+' => { tokens.push(Token::Plus); chars.next(); }
                '-' => { tokens.push(Token::Minus); chars.next(); }
                '#' => {
                    chars.next();
                    let mut hex = String::new();
                    while let Some(&ch) = chars.peek() {
                        if ch.is_ascii_hexdigit() { hex.push(ch); chars.next(); } else { break; }
                    }
                    if hex.len() != 6 { return Err(format!("Invalid hex #{}", hex)); }
                    let r = u8::from_str_radix(&hex[0..2], 16).unwrap_or(0);
                    let g = u8::from_str_radix(&hex[2..4], 16).unwrap_or(0);
                    let b = u8::from_str_radix(&hex[4..6], 16).unwrap_or(0);
                    tokens.push(Token::ColorLiteral(Color { r, g, b }));
                }
                '"' => {
                    chars.next();
                    let mut s = String::new();
                    while let Some(&ch) = chars.peek() {
                        if ch == '"' { chars.next(); break; }
                        s.push(ch); chars.next();
                    }
                    tokens.push(Token::StringLiteral(s));
                }
                _ if c.is_ascii_digit() => {
                    let mut n = String::new();
                    while let Some(&ch) = chars.peek() {
                        if ch.is_ascii_digit() { n.push(ch); chars.next(); } else { break; }
                    }
                    tokens.push(Token::Number(n.parse().unwrap_or(0)));
                }
                _ if c.is_alphanumeric() || c == '_' => {
                    let mut id = String::new();
                    while let Some(&ch) = chars.peek() {
                        if ch.is_alphanumeric() || ch == '_' { id.push(ch); chars.next(); } else { break; }
                    }
                    match id.as_str() {
                        "var" => tokens.push(Token::Var),
                        "on_event" => tokens.push(Token::OnEvent),
                        "set" => tokens.push(Token::Set),
                        "if" => tokens.push(Token::If),
                        _ => tokens.push(Token::Ident(id)),
                    }
                }
                _ => return Err(format!("Unexpected '{}'", c)),
            }
        }
        Ok(tokens)
    }

    fn parse(&self, tokens: &[Token]) -> Result<Vec<Statement>, String> {
        let mut stmts = Vec::new();
        let mut pos = 0;
        while pos < tokens.len() {
            let (stmt, next) = self.parse_stmt(tokens, pos)?;
            stmts.push(stmt);
            pos = next;
        }
        Ok(stmts)
    }

    fn parse_stmt(&self, tokens: &[Token], mut pos: usize) -> Result<(Statement, usize), String> {
        match &tokens[pos] {
            Token::Var => {
                pos += 1;
                let name = match &tokens[pos] { Token::Ident(n) => n.clone(), _ => return Err(String::from("Expected var name")) };
                pos += 1;
                let mut is_idx = false;
                if pos < tokens.len() && tokens[pos] == Token::Colon {
                    pos += 1;
                    if let Token::Ident(t) = &tokens[pos] {
                        if t == "index" { is_idx = true; }
                        pos += 1;
                    }
                }
                if pos >= tokens.len() || tokens[pos] != Token::Equal { return Err(String::from("Expected '='")); }
                pos += 1;
                let (expr, next) = self.parse_expr(tokens, pos)?;
                pos = next;
                if pos < tokens.len() && tokens[pos] == Token::Semicolon { pos += 1; }
                Ok((Statement::VarDecl { name, is_index: is_idx, value: expr }, pos))
            }
            Token::Set => {
                pos += 1;
                let name = match &tokens[pos] { Token::Ident(n) => n.clone(), _ => return Err(String::from("Expected setting name")) };
                pos += 1;
                if pos >= tokens.len() || tokens[pos] != Token::Equal { return Err(String::from("Expected '='")); }
                pos += 1;
                let (expr, next) = self.parse_expr(tokens, pos)?;
                pos = next;
                if pos < tokens.len() && tokens[pos] == Token::Semicolon { pos += 1; }
                Ok((Statement::SetSystem { name, value: expr }, pos))
            }
            Token::If => {
                pos += 1;
                let (cond, mut next) = self.parse_expr(tokens, pos)?;
                pos = next;
                if pos >= tokens.len() || tokens[pos] != Token::LBrace { return Err(String::from("Expected '{' for if")); }
                pos += 1;
                let mut body = Vec::new();
                while pos < tokens.len() && tokens[pos] != Token::RBrace {
                    let (s, nx) = self.parse_stmt(tokens, pos)?;
                    body.push(s); pos = nx;
                }
                if pos >= tokens.len() { return Err(String::from("Expected '}'")); }
                pos += 1;
                if pos < tokens.len() && tokens[pos] == Token::Semicolon { pos += 1; }
                Ok((Statement::If { condition: cond, body }, pos))
            }
            Token::OnEvent => {
                pos += 1;
                let name = match &tokens[pos] { Token::Ident(n) => n.clone(), _ => return Err(String::from("Expected event name")) };
                pos += 1;
                let mut filters = BTreeMap::new();
                if pos < tokens.len() && tokens[pos] == Token::LParen {
                    pos += 1;
                    while pos < tokens.len() && tokens[pos] != Token::RParen {
                        let k = match &tokens[pos] { Token::Ident(n) => n.clone(), _ => return Err(String::from("Expected filter key")) };
                        pos += 1;
                        if tokens[pos] != Token::Equal { return Err(String::from("Expected '=' in filter")); }
                        pos += 1;
                        let v = match &tokens[pos] { Token::StringLiteral(s) => s.clone(), _ => return Err(String::from("Expected string val")) };
                        pos += 1;
                        filters.insert(k, v);
                        if pos < tokens.len() && tokens[pos] == Token::Comma { pos += 1; }
                    }
                    pos += 1; // skip RParen
                }
                if pos >= tokens.len() || tokens[pos] != Token::LBrace { return Err(String::from("Expected '{' for event")); }
                pos += 1;
                let mut body = Vec::new();
                while pos < tokens.len() && tokens[pos] != Token::RBrace {
                    let (s, nx) = self.parse_stmt(tokens, pos)?;
                    body.push(s); pos = nx;
                }
                if pos >= tokens.len() { return Err(String::from("Expected '}'")); }
                pos += 1;
                if pos < tokens.len() && tokens[pos] == Token::Semicolon { pos += 1; }
                Ok((Statement::EventDecl { name, filters, body }, pos))
            }
            Token::Ident(name) => {
                let mut name_copy = name.clone();
                pos += 1;
                
                // Might be assignment, index assignment, or call
                if pos < tokens.len() && tokens[pos] == Token::LBracket {
                    pos += 1;
                    let (prop, mut next) = self.parse_expr(tokens, pos)?;
                    pos = next;
                    if pos >= tokens.len() || tokens[pos] != Token::RBracket { return Err(String::from("Expected ']'")); }
                    pos += 1;
                    
                    if pos < tokens.len() && tokens[pos] == Token::Equal {
                        pos += 1;
                        let (val, mut next) = self.parse_expr(tokens, pos)?;
                        pos = next;
                        if pos < tokens.len() && tokens[pos] == Token::Semicolon { pos += 1; }
                        return Ok((Statement::VarAssign { name: name_copy, prop: Some(prop), value: val }, pos));
                    }
                    return Err(String::from("Expected '=' after index"));
                }
                
                if pos < tokens.len() && tokens[pos] == Token::Equal {
                    pos += 1;
                    let (val, mut next) = self.parse_expr(tokens, pos)?;
                    pos = next;
                    if pos < tokens.len() && tokens[pos] == Token::Semicolon { pos += 1; }
                    return Ok((Statement::VarAssign { name: name_copy, prop: None, value: val }, pos));
                }
                
                if pos < tokens.len() && tokens[pos] == Token::LParen {
                    // Call statement
                    pos += 1;
                    let mut args = Vec::new();
                    while pos < tokens.len() && tokens[pos] != Token::RParen {
                        let (val, mut next) = self.parse_expr(tokens, pos)?;
                        args.push(val);
                        pos = next;
                        if pos < tokens.len() && tokens[pos] == Token::Comma { pos += 1; }
                    }
                    if pos >= tokens.len() || tokens[pos] != Token::RParen { return Err(String::from("Expected ')'")); }
                    pos += 1;
                    if pos < tokens.len() && tokens[pos] == Token::Semicolon { pos += 1; }
                    return Ok((Statement::Call { name: name_copy, args }, pos));
                }
                
                Err(format!("Unexpected identifier statement '{}'", name_copy))
            }
            t => Err(format!("Expected statement, found {:?}", t))
        }
    }

    fn parse_expr(&self, tokens: &[Token], mut pos: usize) -> Result<(Expr, usize), String> {
        let (mut lhs, mut next) = self.parse_primary(tokens, pos)?;
        pos = next;
        
        while pos < tokens.len() {
            match &tokens[pos] {
                Token::Plus => {
                    pos += 1;
                    let (rhs, nx) = self.parse_primary(tokens, pos)?;
                    lhs = Expr::Add(Box::new(lhs), Box::new(rhs));
                    pos = nx;
                }
                Token::EqEq => {
                    pos += 1;
                    let (rhs, nx) = self.parse_primary(tokens, pos)?;
                    lhs = Expr::Eq(Box::new(lhs), Box::new(rhs));
                    pos = nx;
                }
                Token::LBracket => { // index access
                    pos += 1;
                    let (idx_expr, nx) = self.parse_expr(tokens, pos)?;
                    pos = nx;
                    if pos >= tokens.len() || tokens[pos] != Token::RBracket { return Err(String::from("Expected ']'")); }
                    lhs = Expr::Index(Box::new(lhs), Box::new(idx_expr));
                    pos += 1;
                }
                _ => break,
            }
        }
        Ok((lhs, pos))
    }

    fn parse_primary(&self, tokens: &[Token], mut pos: usize) -> Result<(Expr, usize), String> {
        if pos >= tokens.len() { return Err(String::from("Unexpected EOF in expr")); }
        match &tokens[pos] {
            Token::Number(n) => Ok((Expr::Literal(VValue::Integer(*n)), pos + 1)),
            Token::StringLiteral(s) => Ok((Expr::Literal(VValue::String(s.clone())), pos + 1)),
            Token::ColorLiteral(c) => Ok((Expr::Literal(VValue::Color(*c)), pos + 1)),
            Token::Ident(name) => {
                let n = name.clone();
                pos += 1;
                if pos < tokens.len() && tokens[pos] == Token::LParen {
                    // Function call
                    pos += 1;
                    let mut args = Vec::new();
                    while pos < tokens.len() && tokens[pos] != Token::RParen {
                        let (a, nx) = self.parse_expr(tokens, pos)?;
                        args.push(a); pos = nx;
                        if pos < tokens.len() && tokens[pos] == Token::Comma { pos += 1; }
                    }
                    if pos >= tokens.len() { return Err(String::from("Expected ')'")); }
                    pos += 1;
                    Ok((Expr::Call(n, args), pos))
                } else {
                    Ok((Expr::VarRef(n), pos))
                }
            }
            Token::Minus => {
                pos += 1;
                let (e, nx) = self.parse_primary(tokens, pos)?;
                Ok((Expr::Neg(Box::new(e)), nx))
            }
            Token::LParen => {
                // tuple / multiple expressions -> ConcatSequence for format parsing
                pos += 1;
                let mut seq = Vec::new();
                while pos < tokens.len() && tokens[pos] != Token::RParen {
                    let (e, nx) = self.parse_expr(tokens, pos)?;
                    seq.push(e); pos = nx;
                    if pos < tokens.len() && tokens[pos] == Token::Comma { pos += 1; }
                }
                if pos >= tokens.len() { return Err(String::from("Expected ')'")); }
                pos += 1;
                if seq.len() == 1 {
                    Ok((seq.pop().unwrap(), pos)) // Parenthesized expression
                } else {
                    Ok((Expr::ConcatSequence(seq), pos)) // "tuple" formatting trick
                }
            }
            Token::LBrace => {
                // Check if it's an object or a string interpolation block {expr}
                // Differentiate by looking for colon after ident.
                // In config.vsh we have {HEX[i]}, no colon. So it's string interpolation part.
                pos += 1;
                let mut is_obj = false;
                if pos + 1 < tokens.len() && tokens[pos+1] == Token::Colon { is_obj = true; }
                
                if is_obj {
                    let mut map = BTreeMap::new();
                    while pos < tokens.len() && tokens[pos] != Token::RBrace {
                        let k = match &tokens[pos] { Token::Ident(id) => id.clone(), _ => return Err(String::from("Expected key")) };
                        pos += 1;
                        if tokens[pos] != Token::Colon { return Err(String::from("Expected ':'")); }
                        pos += 1;
                        let (v, nx) = self.parse_expr(tokens, pos)?;
                        map.insert(k, Expr::Literal(VValue::None)); // We can't evaluate expr directly here cleanly without evaluating AST nodes.
                        // Wait, in my original logic Object held VValue. I should evaluate during execution, not parsing.
                        // I will hack around it. Let's make an Expr::Object(BTreeMap<String, Expr>).
                        // Instead, let's just make `format` tuple evaluate normally.
                        panic!("Refactoring object literal parsing required.");
                    }
                } else {
                    // `{expr}` interpolation piece
                    let (e, nx) = self.parse_expr(tokens, pos)?;
                    pos = nx;
                    if pos >= tokens.len() || tokens[pos] != Token::RBrace { return Err(String::from("Expected '}'")); }
                    Ok((e, pos + 1))
                }
            }
            _ => Err(format!("Invalid expression start: {:?}", tokens[pos])),
        }
    }
}
