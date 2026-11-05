//! Small math expressions for skill numbers and lore placeholders.
//!
//! ```text
//! $damage + self.magic_power * 0.25
//! max(2, self.mana / 10)
//! ```
//! Numbers, `$params`, `self.<stat>`, `+ - * /`, parentheses and the
//! functions min, max, clamp, floor, ceil, round and abs.

use std::collections::BTreeMap;

pub type Params = BTreeMap<String, f64>;
/// A player's stats, keyed by snake_case name ("magic_power").
pub type PlayerStats = BTreeMap<String, f64>;

/// What `$name` and `self.name` mean while evaluating.
#[derive(Clone, Copy)]
pub struct Scope<'a> {
    pub params: &'a Params,
    pub stats: &'a PlayerStats,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Expr {
    Num(f64),
    Param(String),
    Stat(String),
    Neg(Box<Expr>),
    Bin(Box<Expr>, Op, Box<Expr>),
    Call(Func, Vec<Expr>),
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Op {
    Add,
    Sub,
    Mul,
    Div,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Func {
    Min,
    Max,
    Clamp,
    Floor,
    Ceil,
    Round,
    Abs,
}

impl Expr {
    pub fn eval(&self, scope: Scope) -> f64 {
        match self {
            Expr::Num(v) => *v,
            Expr::Param(name) => scope.params.get(name).copied().unwrap_or(0.0),
            Expr::Stat(name) => scope.stats.get(name).copied().unwrap_or(0.0),
            Expr::Neg(e) => -e.eval(scope),
            Expr::Bin(a, op, b) => {
                let (a, b) = (a.eval(scope), b.eval(scope));
                match op {
                    Op::Add => a + b,
                    Op::Sub => a - b,
                    Op::Mul => a * b,
                    Op::Div if b == 0.0 => 0.0,
                    Op::Div => a / b,
                }
            }
            Expr::Call(f, args) => {
                let v: Vec<f64> = args.iter().map(|a| a.eval(scope)).collect();
                match f {
                    Func::Min => v[0].min(v[1]),
                    Func::Max => v[0].max(v[1]),
                    Func::Clamp => v[0].clamp(v[1].min(v[2]), v[2].max(v[1])),
                    Func::Floor => v[0].floor(),
                    Func::Ceil => v[0].ceil(),
                    Func::Round => v[0].round(),
                    Func::Abs => v[0].abs(),
                }
            }
        }
    }

    /// Calls `f` for every `$param` and `self.stat` used.
    pub fn visit(&self, f: &mut impl FnMut(&Expr)) {
        f(self);
        match self {
            Expr::Neg(e) => e.visit(f),
            Expr::Bin(a, _, b) => {
                a.visit(f);
                b.visit(f);
            }
            Expr::Call(_, args) => args.iter().for_each(|a| a.visit(f)),
            _ => {}
        }
    }
}

/// A parse error, with the char position inside the expression text.
#[derive(Debug, Clone, PartialEq)]
pub struct ExprError {
    pub at: usize,
    pub message: String,
}

pub fn parse(src: &str) -> Result<Expr, ExprError> {
    let mut p = P { chars: src.chars().collect(), i: 0 };
    let e = p.expr()?;
    p.skip_ws();
    if p.i < p.chars.len() {
        return Err(p.error(format!("unexpected `{}`", p.chars[p.i])));
    }
    Ok(e)
}

struct P {
    chars: Vec<char>,
    i: usize,
}

impl P {
    fn error(&self, message: impl Into<String>) -> ExprError {
        ExprError { at: self.i, message: message.into() }
    }

    fn skip_ws(&mut self) {
        while self.chars.get(self.i).is_some_and(|c| c.is_whitespace()) {
            self.i += 1;
        }
    }

    fn eat(&mut self, c: char) -> bool {
        self.skip_ws();
        if self.chars.get(self.i) == Some(&c) {
            self.i += 1;
            true
        } else {
            false
        }
    }

    fn ident(&mut self) -> String {
        let start = self.i;
        // No `-` in names: `$damage-2` must mean `$damage - 2`.
        while self.chars.get(self.i).is_some_and(|c| c.is_ascii_alphanumeric() || *c == '_') {
            self.i += 1;
        }
        self.chars[start..self.i].iter().collect()
    }

    fn expr(&mut self) -> Result<Expr, ExprError> {
        let mut left = self.term()?;
        loop {
            let op = if self.eat('+') {
                Op::Add
            } else if self.eat('-') {
                Op::Sub
            } else {
                return Ok(left);
            };
            left = Expr::Bin(Box::new(left), op, Box::new(self.term()?));
        }
    }

    fn term(&mut self) -> Result<Expr, ExprError> {
        let mut left = self.unary()?;
        loop {
            let op = if self.eat('*') {
                Op::Mul
            } else if self.eat('/') {
                Op::Div
            } else {
                return Ok(left);
            };
            left = Expr::Bin(Box::new(left), op, Box::new(self.unary()?));
        }
    }

    fn unary(&mut self) -> Result<Expr, ExprError> {
        if self.eat('-') {
            return Ok(Expr::Neg(Box::new(self.unary()?)));
        }
        self.atom()
    }

    fn atom(&mut self) -> Result<Expr, ExprError> {
        self.skip_ws();
        let Some(&c) = self.chars.get(self.i) else {
            return Err(self.error("expected a number, $param or self.stat"));
        };
        if c == '(' {
            self.i += 1;
            let e = self.expr()?;
            if !self.eat(')') {
                return Err(self.error("expected `)`"));
            }
            return Ok(e);
        }
        if c.is_ascii_digit() || c == '.' {
            let start = self.i;
            while self.chars.get(self.i).is_some_and(|c| c.is_ascii_digit() || *c == '.') {
                self.i += 1;
            }
            let text: String = self.chars[start..self.i].iter().collect();
            return text.parse().map(Expr::Num).map_err(|_| ExprError { at: start, message: format!("bad number `{text}`") });
        }
        if c == '$' {
            self.i += 1;
            let name = self.ident();
            if name.is_empty() {
                return Err(self.error("expected a param name after `$`"));
            }
            return Ok(Expr::Param(name));
        }
        let start = self.i;
        let name = self.ident();
        if name.is_empty() {
            return Err(self.error(format!("unexpected `{c}`")));
        }
        if name == "self" {
            if !self.eat('.') {
                return Err(self.error("expected `.` after `self`, like self.magic_power"));
            }
            let stat = self.ident();
            if stat.is_empty() {
                return Err(self.error("expected a stat name after `self.`"));
            }
            // `self.damage.fire`: the part of a stat tagged with an element.
            if self.eat('.') {
                let element = self.ident();
                if element.is_empty() {
                    return Err(self.error("expected an element after the stat, like self.magic_power.fire"));
                }
                return Ok(Expr::Stat(format!("{stat}.{element}")));
            }
            return Ok(Expr::Stat(stat));
        }
        let (func, arity) = match name.as_str() {
            "min" => (Func::Min, 2),
            "max" => (Func::Max, 2),
            "clamp" => (Func::Clamp, 3),
            "floor" => (Func::Floor, 1),
            "ceil" => (Func::Ceil, 1),
            "round" => (Func::Round, 1),
            "abs" => (Func::Abs, 1),
            _ => {
                return Err(ExprError {
                    at: start,
                    message: format!("unknown name `{name}` (use $param, self.stat or min/max/clamp/floor/ceil/round/abs)"),
                });
            }
        };
        if !self.eat('(') {
            return Err(self.error(format!("expected `(` after `{name}`")));
        }
        let mut args = vec![self.expr()?];
        while self.eat(',') {
            args.push(self.expr()?);
        }
        if !self.eat(')') {
            return Err(self.error("expected `)`"));
        }
        if args.len() != arity {
            return Err(ExprError { at: start, message: format!("`{name}` takes {arity} value(s), got {}", args.len()) });
        }
        Ok(Expr::Call(func, args))
    }
}

/// A piece of lore text: plain text or a `<expr>` placeholder.
#[derive(Debug, Clone, PartialEq)]
pub enum Piece {
    Text(String),
    Expr(Expr),
}

/// Splits text into plain parts and `<expr>` placeholders.
/// Errors point at the char position in `text`.
pub fn pieces(text: &str) -> Result<Vec<Piece>, ExprError> {
    let mut out = Vec::new();
    let mut rest = text;
    let mut offset = 0;
    while let Some(open) = rest.find('<') {
        let Some(close) = rest[open..].find('>') else {
            return Err(ExprError { at: offset + rest[..open].chars().count(), message: "`<` without a closing `>`".into() });
        };
        if open > 0 {
            out.push(Piece::Text(rest[..open].to_string()));
        }
        let inner = &rest[open + 1..open + close];
        let inner_at = offset + rest[..open + 1].chars().count();
        let e = parse(inner).map_err(|e| ExprError { at: inner_at + e.at, message: e.message })?;
        out.push(Piece::Expr(e));
        offset += rest[..open + close + 1].chars().count();
        rest = &rest[open + close + 1..];
    }
    if !rest.is_empty() {
        out.push(Piece::Text(rest.to_string()));
    }
    Ok(out)
}

/// 12 -> "12", 12.5 -> "12.5", 12.345 -> "12.3".
pub fn format_number(v: f64) -> String {
    let rounded = (v * 10.0).round() / 10.0;
    if rounded.fract() == 0.0 { format!("{}", rounded as i64) } else { format!("{rounded:.1}") }
}

/// Fills in `<expr>` placeholders. Text that fails to parse is left as is
/// (the loader already reported it).
pub fn render(text: &str, scope: Scope) -> String {
    match pieces(text) {
        Ok(pieces) => pieces
            .iter()
            .map(|p| match p {
                Piece::Text(t) => t.clone(),
                Piece::Expr(e) => format_number(e.eval(scope)),
            })
            .collect(),
        Err(_) => text.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn eval(src: &str) -> f64 {
        let params = Params::from([("damage".into(), 10.0)]);
        let stats = PlayerStats::from([("magic_power".into(), 50.0)]);
        parse(src).unwrap().eval(Scope { params: &params, stats: &stats })
    }

    #[test]
    fn math() {
        assert_eq!(eval("1 + 2 * 3"), 7.0);
        assert_eq!(eval("(1 + 2) * 3"), 9.0);
        assert_eq!(eval("-2 - -3"), 1.0);
        assert_eq!(eval("$damage + self.magic_power * 0.25"), 22.5);
        assert_eq!(eval("$damage-2"), 8.0);
        assert_eq!(eval("self.unknown"), 0.0);
        assert_eq!(eval("max(2, min(5, 9)) + floor(1.7) + clamp(15, 0, 10)"), 16.0);
        assert_eq!(eval("1 / 0"), 0.0);
    }

    #[test]
    fn errors() {
        assert_eq!(parse("1 +").unwrap_err().message, "expected a number, $param or self.stat");
        assert!(parse("power * 2").unwrap_err().message.starts_with("unknown name `power`"));
        assert_eq!(parse("max(1)").unwrap_err().message, "`max` takes 2 value(s), got 1");
        assert_eq!(parse("2 3").unwrap_err().at, 2);
    }

    #[test]
    fn placeholders() {
        let params = Params::from([("damage".into(), 10.0)]);
        let stats = PlayerStats::from([("magic_power".into(), 50.0)]);
        let scope = Scope { params: &params, stats: &stats };
        assert_eq!(render("&7Deals &5<$damage + self.magic_power * 0.25>&7.", scope), "&7Deals &522.5&7.");
        assert_eq!(render("no placeholders", scope), "no placeholders");
        assert_eq!(pieces("a <1 +> b").unwrap_err().at, 6);
        assert!(pieces("a < b").is_err());
    }
}
