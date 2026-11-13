//! 条件式と変数の変更式。構文解析と、変数の存在・型の検査はすべてプログラムで行う。
//!
//! 条件式の例: `好感度 >= 3 and not 鍵を持っている`、`(所持金 > 100 or 王の許可) and 章 == 2`
//! 変更式の例: `好感度 += 1; 鍵を持っている = true; 呼び名 = "先輩"`
//!
//! - 演算子: `and` `or` `not`（`&&` `||` `!` も可）、`==` `!=` `<` `<=` `>` `>=`、`+` `-`
//! - 値: 整数・小数、`true` / `false`、`"文字列"`、変数名（英字・日本語・数字・`_`。数字始まりは不可）
//! - 変更: `=`（代入）、`+=`、`-=`（数値のみ）。複数は `;` で区切る

use std::collections::HashMap;
use std::fmt;

use serde::Serialize;

/// 変数の型。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum VarType {
    Number,
    Bool,
    Text,
}

impl VarType {
    pub fn parse(s: &str) -> Option<VarType> {
        match s {
            "number" => Some(VarType::Number),
            "bool" => Some(VarType::Bool),
            "text" => Some(VarType::Text),
            _ => None,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            VarType::Number => "数値",
            VarType::Bool => "真偽値",
            VarType::Text => "文字列",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
enum Token {
    Number(f64),
    Text(String),
    Ident(String),
    True,
    False,
    And,
    Or,
    Not,
    Op(&'static str),
    LParen,
    RParen,
    Semicolon,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogicError(pub String);

impl fmt::Display for LogicError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

fn err<T>(msg: impl Into<String>) -> Result<T, LogicError> {
    Err(LogicError(msg.into()))
}

fn is_ident_start(c: char) -> bool {
    c == '_' || (c.is_alphabetic())
}

fn is_ident_char(c: char) -> bool {
    c == '_' || c.is_alphanumeric()
}

/// 変数名として使える文字列か。
pub fn is_valid_name(name: &str) -> bool {
    let mut chars = name.chars();
    matches!(chars.next(), Some(c) if is_ident_start(c))
        && chars.all(is_ident_char)
        && name.chars().count() <= 50
        && !matches!(name, "and" | "or" | "not" | "true" | "false")
}

fn tokenize(src: &str) -> Result<Vec<Token>, LogicError> {
    let chars: Vec<char> = src.chars().collect();
    let mut i = 0;
    let mut out = Vec::new();
    while i < chars.len() {
        let c = chars[i];
        if c.is_whitespace() {
            i += 1;
            continue;
        }
        let next = chars.get(i + 1).copied();
        let two = |a: char, b: char| c == a && next == Some(b);
        let op = if two('=', '=') {
            Some("==")
        } else if two('!', '=') {
            Some("!=")
        } else if two('<', '=') {
            Some("<=")
        } else if two('>', '=') {
            Some(">=")
        } else if two('+', '=') {
            Some("+=")
        } else if two('-', '=') {
            Some("-=")
        } else if two('&', '&') {
            out.push(Token::And);
            i += 2;
            continue;
        } else if two('|', '|') {
            out.push(Token::Or);
            i += 2;
            continue;
        } else {
            None
        };
        if let Some(op) = op {
            out.push(Token::Op(op));
            i += 2;
            continue;
        }
        match c {
            '(' => out.push(Token::LParen),
            ')' => out.push(Token::RParen),
            ';' => out.push(Token::Semicolon),
            '!' => out.push(Token::Not),
            '<' => out.push(Token::Op("<")),
            '>' => out.push(Token::Op(">")),
            '=' => out.push(Token::Op("=")),
            '+' => out.push(Token::Op("+")),
            '-' => out.push(Token::Op("-")),
            '"' | '「' => {
                let close = if c == '"' { '"' } else { '」' };
                let start = i + 1;
                let Some(len) = chars[start..].iter().position(|&ch| ch == close) else {
                    return err("文字列の閉じ記号がありません");
                };
                out.push(Token::Text(chars[start..start + len].iter().collect()));
                i = start + len + 1;
                continue;
            }
            d if d.is_ascii_digit() => {
                let start = i;
                while i < chars.len() && (chars[i].is_ascii_digit() || chars[i] == '.') {
                    i += 1;
                }
                let text: String = chars[start..i].iter().collect();
                let n = text.parse::<f64>().map_err(|_| LogicError(format!("数値の書き方が正しくありません（{text}）")))?;
                out.push(Token::Number(n));
                continue;
            }
            ch if is_ident_start(ch) => {
                let start = i;
                while i < chars.len() && is_ident_char(chars[i]) {
                    i += 1;
                }
                let word: String = chars[start..i].iter().collect();
                out.push(match word.as_str() {
                    "and" => Token::And,
                    "or" => Token::Or,
                    "not" => Token::Not,
                    "true" => Token::True,
                    "false" => Token::False,
                    _ => Token::Ident(word),
                });
                continue;
            }
            other => return err(format!("使えない文字があります（{other}）")),
        }
        i += 1;
    }
    Ok(out)
}

/// 構文木。
#[derive(Debug, Clone, PartialEq)]
pub enum Expr {
    Number(f64),
    Text(String),
    Bool(bool),
    Var(String),
    Not(Box<Expr>),
    Binary(&'static str, Box<Expr>, Box<Expr>),
}

/// 変数の変更 1 件。
#[derive(Debug, Clone, PartialEq)]
pub struct Effect {
    pub variable: String,
    /// `=` / `+=` / `-=`
    pub op: &'static str,
    pub value: Expr,
}

struct Parser {
    tokens: Vec<Token>,
    pos: usize,
}

impl Parser {
    fn peek(&self) -> Option<&Token> {
        self.tokens.get(self.pos)
    }

    fn next(&mut self) -> Option<Token> {
        let t = self.tokens.get(self.pos).cloned();
        self.pos += 1;
        t
    }

    fn or(&mut self) -> Result<Expr, LogicError> {
        let mut left = self.and()?;
        while self.peek() == Some(&Token::Or) {
            self.pos += 1;
            left = Expr::Binary("or", Box::new(left), Box::new(self.and()?));
        }
        Ok(left)
    }

    fn and(&mut self) -> Result<Expr, LogicError> {
        let mut left = self.not()?;
        while self.peek() == Some(&Token::And) {
            self.pos += 1;
            left = Expr::Binary("and", Box::new(left), Box::new(self.not()?));
        }
        Ok(left)
    }

    fn not(&mut self) -> Result<Expr, LogicError> {
        if self.peek() == Some(&Token::Not) {
            self.pos += 1;
            return Ok(Expr::Not(Box::new(self.not()?)));
        }
        self.cmp()
    }

    fn cmp(&mut self) -> Result<Expr, LogicError> {
        let left = self.sum()?;
        if let Some(Token::Op(op @ ("==" | "!=" | "<" | "<=" | ">" | ">="))) = self.peek().cloned() {
            self.pos += 1;
            let right = self.sum()?;
            return Ok(Expr::Binary(op, Box::new(left), Box::new(right)));
        }
        if let Some(Token::Op("=")) = self.peek() {
            return err("比較には「=」ではなく「==」を使ってください");
        }
        Ok(left)
    }

    fn sum(&mut self) -> Result<Expr, LogicError> {
        let mut left = self.atom()?;
        while let Some(Token::Op(op @ ("+" | "-"))) = self.peek().cloned() {
            self.pos += 1;
            left = Expr::Binary(op, Box::new(left), Box::new(self.atom()?));
        }
        Ok(left)
    }

    fn atom(&mut self) -> Result<Expr, LogicError> {
        match self.next() {
            Some(Token::Number(n)) => Ok(Expr::Number(n)),
            Some(Token::Text(t)) => Ok(Expr::Text(t)),
            Some(Token::True) => Ok(Expr::Bool(true)),
            Some(Token::False) => Ok(Expr::Bool(false)),
            Some(Token::Ident(name)) => Ok(Expr::Var(name)),
            Some(Token::Op("-")) => match self.next() {
                Some(Token::Number(n)) => Ok(Expr::Number(-n)),
                _ => err("「-」の後には数値を書いてください"),
            },
            Some(Token::LParen) => {
                let e = self.or()?;
                if self.next() != Some(Token::RParen) {
                    return err("「(」に対応する「)」がありません");
                }
                Ok(e)
            }
            Some(t) => err(format!("ここに「{}」は書けません", describe(&t))),
            None => err("式が途中で終わっています"),
        }
    }
}

fn describe(t: &Token) -> String {
    match t {
        Token::Number(n) => n.to_string(),
        Token::Text(s) => format!("\"{s}\""),
        Token::Ident(s) => s.clone(),
        Token::True => "true".into(),
        Token::False => "false".into(),
        Token::And => "and".into(),
        Token::Or => "or".into(),
        Token::Not => "not".into(),
        Token::Op(o) => (*o).into(),
        Token::LParen => "(".into(),
        Token::RParen => ")".into(),
        Token::Semicolon => ";".into(),
    }
}

/// 条件式を解析する。空文字列は「条件なし」として `None`。
pub fn parse_condition(src: &str) -> Result<Option<Expr>, LogicError> {
    let tokens = tokenize(src)?;
    if tokens.is_empty() {
        return Ok(None);
    }
    let mut p = Parser { tokens, pos: 0 };
    let e = p.or()?;
    if let Some(t) = p.peek() {
        return err(format!("「{}」以降を解釈できません", describe(t)));
    }
    Ok(Some(e))
}

/// 変更式を解析する。
pub fn parse_effects(src: &str) -> Result<Vec<Effect>, LogicError> {
    let tokens = tokenize(src)?;
    let mut out = Vec::new();
    for stmt in tokens.split(|t| *t == Token::Semicolon) {
        if stmt.is_empty() {
            continue;
        }
        let (Token::Ident(name), Some(Token::Op(op @ ("=" | "+=" | "-=")))) = (&stmt[0], stmt.get(1)) else {
            return err("変更は「変数名 = 値」「変数名 += 数値」「変数名 -= 数値」の形で書いてください");
        };
        let mut p = Parser { tokens: stmt[2..].to_vec(), pos: 0 };
        let value = p.or()?;
        if let Some(t) = p.peek() {
            return err(format!("「{}」以降を解釈できません", describe(t)));
        }
        out.push(Effect { variable: name.clone(), op, value });
    }
    Ok(out)
}

/// 式で使われている変数名。
pub fn variables_in(e: &Expr, out: &mut Vec<String>) {
    match e {
        Expr::Var(v) => {
            if !out.contains(v) {
                out.push(v.clone());
            }
        }
        Expr::Not(inner) => variables_in(inner, out),
        Expr::Binary(_, a, b) => {
            variables_in(a, out);
            variables_in(b, out);
        }
        _ => {}
    }
}

/// 型を検査する。未定義の変数や型の不一致を、ユーザー向けの文で返す。
pub fn type_of(e: &Expr, vars: &HashMap<String, VarType>, problems: &mut Vec<String>) -> Option<VarType> {
    match e {
        Expr::Number(_) => Some(VarType::Number),
        Expr::Text(_) => Some(VarType::Text),
        Expr::Bool(_) => Some(VarType::Bool),
        Expr::Var(name) => match vars.get(name) {
            Some(t) => Some(*t),
            None => {
                problems.push(format!("変数「{name}」は定義されていません"));
                None
            }
        },
        Expr::Not(inner) => {
            if let Some(t) = type_of(inner, vars, problems) {
                if t != VarType::Bool {
                    problems.push(format!("not の対象が{}です（真偽値が必要です）", t.label()));
                }
            }
            Some(VarType::Bool)
        }
        Expr::Binary(op, a, b) => {
            let (ta, tb) = (type_of(a, vars, problems), type_of(b, vars, problems));
            match *op {
                "and" | "or" => {
                    for t in [ta, tb].into_iter().flatten() {
                        if t != VarType::Bool {
                            problems.push(format!("{op} の対象が{}です（真偽値が必要です）", t.label()));
                        }
                    }
                    Some(VarType::Bool)
                }
                "==" | "!=" => {
                    if let (Some(x), Some(y)) = (ta, tb) {
                        if x != y {
                            problems.push(format!("{}と{}を比較しています", x.label(), y.label()));
                        }
                    }
                    Some(VarType::Bool)
                }
                "<" | "<=" | ">" | ">=" => {
                    for t in [ta, tb].into_iter().flatten() {
                        if t != VarType::Number {
                            problems.push(format!("大小の比較に{}を使っています（数値が必要です）", t.label()));
                        }
                    }
                    Some(VarType::Bool)
                }
                "+" | "-" => {
                    if op == &"+" && ta == Some(VarType::Text) && tb == Some(VarType::Text) {
                        return Some(VarType::Text);
                    }
                    for t in [ta, tb].into_iter().flatten() {
                        if t != VarType::Number {
                            problems.push(format!("「{op}」の対象が{}です（数値が必要です）", t.label()));
                        }
                    }
                    Some(VarType::Number)
                }
                _ => None,
            }
        }
    }
}

/// 条件式を検査する。問題がなければ空。
pub fn check_condition(src: &str, vars: &HashMap<String, VarType>) -> Vec<String> {
    match parse_condition(src) {
        Err(e) => vec![format!("条件式の書き方が正しくありません: {e}")],
        Ok(None) => vec![],
        Ok(Some(expr)) => {
            let mut problems = Vec::new();
            if let Some(t) = type_of(&expr, vars, &mut problems) {
                if t != VarType::Bool {
                    problems.push(format!("条件式の結果が{}です（真偽値になる式にしてください）", t.label()));
                }
            }
            problems
        }
    }
}

/// 変更式を検査する。
pub fn check_effects(src: &str, vars: &HashMap<String, VarType>) -> Vec<String> {
    match parse_effects(src) {
        Err(e) => vec![format!("変更式の書き方が正しくありません: {e}")],
        Ok(effects) => {
            let mut problems = Vec::new();
            for eff in effects {
                let target = vars.get(&eff.variable).copied();
                if target.is_none() {
                    problems.push(format!("変数「{}」は定義されていません", eff.variable));
                }
                let value = type_of(&eff.value, vars, &mut problems);
                if let (Some(t), Some(v)) = (target, value) {
                    if eff.op != "=" && t != VarType::Number {
                        problems.push(format!("「{}」は{}のため「{}」は使えません", eff.variable, t.label(), eff.op));
                    } else if t != v {
                        problems.push(format!("「{}」（{}）に{}を設定しています", eff.variable, t.label(), v.label()));
                    }
                }
            }
            problems
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vars() -> HashMap<String, VarType> {
        HashMap::from([
            ("好感度".to_string(), VarType::Number),
            ("has_key".to_string(), VarType::Bool),
            ("呼び名".to_string(), VarType::Text),
        ])
    }

    #[test]
    fn parses_conditions() {
        assert_eq!(parse_condition("  ").unwrap(), None);
        let e = parse_condition("好感度 >= 3 and not has_key || 呼び名 == \"先輩\"").unwrap().unwrap();
        let mut names = Vec::new();
        variables_in(&e, &mut names);
        assert_eq!(names, ["好感度", "has_key", "呼び名"]);
        assert!(check_condition("好感度 >= 3 and not has_key || 呼び名 == \"先輩\"", &vars()).is_empty());
        assert!(check_condition("(好感度 > -1 or has_key) and 好感度 + 2 < 10", &vars()).is_empty());
        assert!(check_condition("呼び名 == 「先輩」", &vars()).is_empty());
    }

    #[test]
    fn reports_condition_problems() {
        let v = vars();
        assert!(check_condition("好感度 >= ", &v)[0].contains("途中で終わって"));
        assert!(check_condition("好感度 = 3", &v)[0].contains("=="));
        assert!(check_condition("(好感度 > 1", &v)[0].contains(")"));
        assert!(check_condition("所持金 > 1", &v)[0].contains("「所持金」は定義されていません"));
        assert!(check_condition("呼び名 > 1", &v)[0].contains("大小の比較"));
        assert!(check_condition("好感度", &v)[0].contains("真偽値"));
        assert!(check_condition("has_key == 1", &v)[0].contains("比較"));
        assert!(check_condition("好感度 > 1 #", &v)[0].contains("使えない文字"));
    }

    #[test]
    fn parses_and_checks_effects() {
        let v = vars();
        let effects = parse_effects("好感度 += 1; has_key = true;; 呼び名 = \"先輩\"").unwrap();
        assert_eq!(effects.len(), 3);
        assert_eq!(effects[0].op, "+=");
        assert!(check_effects("好感度 += 1; has_key = true; 呼び名 = \"先輩\"", &v).is_empty());
        assert!(check_effects("好感度 = 好感度 - 2", &v).is_empty());
        assert!(check_effects("has_key += 1", &v)[0].contains("使えません"));
        assert!(check_effects("呼び名 = 3", &v)[0].contains("文字列"));
        assert!(check_effects("所持金 = 3", &v)[0].contains("定義されていません"));
        assert!(check_effects("好感度 1", &v)[0].contains("変数名 = 値"));
    }

    #[test]
    fn validates_names() {
        assert!(is_valid_name("好感度"));
        assert!(is_valid_name("has_key2"));
        assert!(!is_valid_name("2nd"));
        assert!(!is_valid_name("and"));
        assert!(!is_valid_name("a b"));
        assert_eq!(VarType::parse("bool"), Some(VarType::Bool));
    }
}
