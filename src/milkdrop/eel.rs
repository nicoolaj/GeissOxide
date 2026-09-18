//! NS-EEL, the expression language of MilkDrop presets, as shipped with MilkDrop 2
//! (`ns-eel2/`): `; `-separated statements, `= + - * / % & |`, parentheses and function calls.
//! `&`/`|` are logical, `%` is an integer modulo, comparisons are functions (`above`, `equal`…).

use std::collections::HashMap;

use anyhow::{Result, bail};
use rand::{RngExt, SeedableRng};

/// Values closer to zero than this are "false" (`NSEEL_CLOSEFACTOR`).
const CLOSE: f64 = 0.000_01;
/// Upper bound on `loop`/`while` iterations (`NSEEL_LOOPFUNC_SUPPORT_MAXLEN`).
const MAX_LOOP: usize = 1_000_000;
/// Cells addressable by `megabuf`/`gmegabuf` (`NSEEL_RAM_BLOCKS * NSEEL_RAM_ITEMSPERBLOCK`).
const MEM_CELLS: f64 = 128.0 * 65536.0;

/// Sparse `megabuf` storage.
pub type Memory = HashMap<u32, f64>;

/// Variables (case-insensitive names → slots), local `megabuf` and RNG of one script scope.
#[derive(Default)]
pub struct Context {
    names: HashMap<String, usize>,
    pub vars: Vec<f64>,
    mem: Memory,
    rng: Option<rand::rngs::SmallRng>,
}

impl Context {
    /// Slot of `name`, registering it (as 0.0) on first use.
    pub fn slot(&mut self, name: &str) -> usize {
        let key = name.to_ascii_lowercase();
        if let Some(&i) = self.names.get(&key) {
            return i;
        }
        self.vars.push(0.0);
        self.names.insert(key, self.vars.len() - 1);
        self.vars.len() - 1
    }

    /// Current value of `name` (0.0 when the variable was never set).
    pub fn get(&self, name: &str) -> f64 {
        self.names
            .get(&name.to_ascii_lowercase())
            .map_or(0.0, |&i| self.vars[i])
    }

    pub fn set(&mut self, name: &str, value: f64) {
        let i = self.slot(name);
        self.vars[i] = value;
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Op {
    Add,
    Sub,
    Mul,
    Div,
    Mod,
    And,
    Or,
}

#[derive(Clone, Debug)]
enum Expr {
    Num(f64),
    Var(usize),
    Mem(Box<Expr>),
    GMem(Box<Expr>),
    Assign(Box<Expr>, Box<Expr>),
    Bin(Op, Box<Expr>, Box<Expr>),
    Neg(Box<Expr>),
    Call(String, Vec<Expr>),
}

/// A compiled script.
#[derive(Clone, Debug, Default)]
pub struct Program {
    stmts: Vec<Expr>,
}

impl Program {
    /// Parses `src`, registering its variables in `ctx`. An empty source is a valid no-op.
    pub fn compile(src: &str, ctx: &mut Context) -> Result<Self> {
        let tokens = lex(src)?;
        let mut parser = Parser {
            tokens,
            pos: 0,
            ctx,
        };
        let mut stmts = Vec::new();
        while parser.pos < parser.tokens.len() {
            if parser.eat(&Token::Semi) {
                continue;
            }
            stmts.push(parser.expr()?);
            if parser.pos < parser.tokens.len() && !parser.eat(&Token::Semi) {
                bail!("expected ';' near token {}", parser.pos);
            }
        }
        Ok(Self { stmts })
    }

    pub fn is_empty(&self) -> bool {
        self.stmts.is_empty()
    }

    /// Runs every statement; `gmem` is the `gmegabuf` shared by all scopes.
    pub fn run(&self, ctx: &mut Context, gmem: &mut Memory) {
        for s in &self.stmts {
            eval(s, ctx, gmem);
        }
    }
}

fn truthy(v: f64) -> bool {
    v.abs() > CLOSE
}

fn eval(e: &Expr, ctx: &mut Context, gmem: &mut Memory) -> f64 {
    match e {
        Expr::Num(v) => *v,
        Expr::Var(i) => ctx.vars[*i],
        Expr::Mem(i) => {
            let i = eval(i, ctx, gmem);
            mem_index(i)
                .and_then(|i| ctx.mem.get(&i).copied())
                .unwrap_or(0.0)
        }
        Expr::GMem(i) => {
            let i = eval(i, ctx, gmem);
            mem_index(i)
                .and_then(|i| gmem.get(&i).copied())
                .unwrap_or(0.0)
        }
        Expr::Assign(target, value) => {
            let v = eval(value, ctx, gmem);
            match &**target {
                Expr::Var(i) => ctx.vars[*i] = v,
                Expr::Mem(i) => {
                    let i = eval(i, ctx, gmem);
                    if let Some(i) = mem_index(i) {
                        ctx.mem.insert(i, v);
                    }
                }
                Expr::GMem(i) => {
                    let i = eval(i, ctx, gmem);
                    if let Some(i) = mem_index(i) {
                        gmem.insert(i, v);
                    }
                }
                _ => {}
            }
            v
        }
        Expr::Bin(op, a, b) => {
            let a = eval(a, ctx, gmem);
            match op {
                // `&` and `|` short-circuit like the original.
                Op::And => f64::from(truthy(a) && truthy(eval(b, ctx, gmem))),
                Op::Or => f64::from(truthy(a) || truthy(eval(b, ctx, gmem))),
                _ => {
                    let b = eval(b, ctx, gmem);
                    match op {
                        Op::Add => a + b,
                        Op::Sub => a - b,
                        Op::Mul => a * b,
                        Op::Div => a / b,
                        Op::Mod => {
                            let (a, b) = (a.abs() as i64, b.abs() as i64);
                            if b == 0 { 0.0 } else { (a % b) as f64 }
                        }
                        Op::And | Op::Or => unreachable!(),
                    }
                }
            }
        }
        Expr::Neg(a) => -eval(a, ctx, gmem),
        Expr::Call(name, args) => call(name, args, ctx, gmem),
    }
}

fn mem_index(i: f64) -> Option<u32> {
    (0.0..MEM_CELLS).contains(&i).then_some(i as u32)
}

fn call(name: &str, args: &[Expr], ctx: &mut Context, gmem: &mut Memory) -> f64 {
    // Lazily evaluated forms first.
    match (name, args.len()) {
        ("if", 3) => {
            let branch = if truthy(eval(&args[0], ctx, gmem)) {
                &args[1]
            } else {
                &args[2]
            };
            return eval(branch, ctx, gmem);
        }
        ("loop", 2) => {
            let n = eval(&args[0], ctx, gmem) as i64;
            for _ in 0..n.clamp(0, MAX_LOOP as i64) {
                eval(&args[1], ctx, gmem);
            }
            return 0.0;
        }
        ("while", 1) => {
            for _ in 0..MAX_LOOP {
                if !truthy(eval(&args[0], ctx, gmem)) {
                    break;
                }
            }
            return 0.0;
        }
        _ => {}
    }
    let v: Vec<f64> = args.iter().map(|a| eval(a, ctx, gmem)).collect();
    let (a, b) = (
        v.first().copied().unwrap_or(0.0),
        v.get(1).copied().unwrap_or(0.0),
    );
    match name {
        "sin" => a.sin(),
        "cos" => a.cos(),
        "tan" => a.tan(),
        "asin" => a.asin(),
        "acos" => a.acos(),
        "atan" => a.atan(),
        "atan2" => a.atan2(b),
        "sqr" => a * a,
        "sqrt" => a.abs().sqrt(),
        "invsqrt" => 1.0 / a.sqrt(),
        "pow" => a.powf(b),
        "exp" => a.exp(),
        "log" => a.ln(),
        "log10" => a.log10(),
        "abs" => a.abs(),
        "min" => a.min(b),
        "max" => a.max(b),
        "sign" => {
            if a > 0.0 {
                1.0
            } else if a < 0.0 {
                -1.0
            } else {
                0.0
            }
        }
        "rand" => {
            let n = a.floor().max(1.0);
            let rng = ctx
                .rng
                .get_or_insert_with(|| rand::rngs::SmallRng::from_rng(&mut rand::rng()));
            rng.random::<f64>() * n
        }
        "int" | "floor" => a.floor(),
        "ceil" => a.ceil(),
        "sigmoid" => {
            let t = 1.0 + (-a * b).exp();
            if t.abs() > CLOSE { 1.0 / t } else { 0.0 }
        }
        "band" => f64::from(truthy(a) && truthy(b)),
        "bor" => f64::from(truthy(a) || truthy(b)),
        "bnot" => f64::from(!truthy(a)),
        "equal" => f64::from((a - b).abs() < CLOSE),
        "above" => f64::from(a > b),
        "below" => f64::from(a < b),
        "exec2" | "exec3" => v.last().copied().unwrap_or(0.0),
        _ => 0.0,
    }
}

#[derive(Clone, Debug, PartialEq)]
enum Token {
    Num(f64),
    Ident(String),
    Op(char),
    LParen,
    RParen,
    Comma,
    Semi,
    Assign,
}

fn lex(src: &str) -> Result<Vec<Token>> {
    let mut tokens = Vec::new();
    let chars: Vec<char> = src.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        match c {
            c if c.is_whitespace() => i += 1,
            '/' if chars.get(i + 1) == Some(&'/') => {
                while i < chars.len() && chars[i] != '\n' {
                    i += 1;
                }
            }
            '0'..='9' | '.' => {
                let start = i;
                while i < chars.len() && (chars[i].is_ascii_digit() || chars[i] == '.') {
                    i += 1;
                }
                let text: String = chars[start..i].iter().collect();
                tokens.push(Token::Num(
                    text.parse().or_else(|_| format!("{text}0").parse())?,
                ));
            }
            c if c.is_ascii_alphabetic() || c == '_' || c == '$' => {
                let start = i;
                while i < chars.len()
                    && (chars[i].is_ascii_alphanumeric() || chars[i] == '_' || chars[i] == '$')
                {
                    i += 1;
                }
                let name: String = chars[start..i]
                    .iter()
                    .collect::<String>()
                    .to_ascii_lowercase();
                tokens.push(match name.as_str() {
                    "$pi" => Token::Num(std::f64::consts::PI),
                    "$e" => Token::Num(std::f64::consts::E),
                    "$phi" => Token::Num(1.618_033_988_749_895),
                    _ => Token::Ident(name),
                });
            }
            '+' | '-' | '*' | '/' | '%' | '&' | '|' => {
                tokens.push(Token::Op(c));
                i += 1;
            }
            '(' => {
                tokens.push(Token::LParen);
                i += 1;
            }
            ')' => {
                tokens.push(Token::RParen);
                i += 1;
            }
            ',' => {
                tokens.push(Token::Comma);
                i += 1;
            }
            ';' => {
                tokens.push(Token::Semi);
                i += 1;
            }
            '=' => {
                tokens.push(Token::Assign);
                i += 1;
            }
            other => bail!("unexpected character {other:?}"),
        }
    }
    Ok(tokens)
}

struct Parser<'a> {
    tokens: Vec<Token>,
    pos: usize,
    ctx: &'a mut Context,
}

impl Parser<'_> {
    fn peek(&self) -> Option<&Token> {
        self.tokens.get(self.pos)
    }

    fn eat(&mut self, t: &Token) -> bool {
        if self.peek() == Some(t) {
            self.pos += 1;
            true
        } else {
            false
        }
    }

    fn eat_op(&mut self, ops: &[char]) -> Option<char> {
        if let Some(Token::Op(c)) = self.peek()
            && ops.contains(c)
        {
            let c = *c;
            self.pos += 1;
            return Some(c);
        }
        None
    }

    /// expr := or ('=' expr)?   — assignment is right-associative.
    fn expr(&mut self) -> Result<Expr> {
        let lhs = self.binary(0)?;
        if self.eat(&Token::Assign) {
            if !matches!(lhs, Expr::Var(_) | Expr::Mem(_) | Expr::GMem(_)) {
                bail!("cannot assign to an expression");
            }
            let rhs = self.expr()?;
            return Ok(Expr::Assign(Box::new(lhs), Box::new(rhs)));
        }
        Ok(lhs)
    }

    /// Precedence climbing over the levels `|`, `&`, `+ -`, `* / %`.
    fn binary(&mut self, level: usize) -> Result<Expr> {
        const LEVELS: [&[char]; 4] = [&['|'], &['&'], &['+', '-'], &['*', '/', '%']];
        if level == LEVELS.len() {
            return self.unary();
        }
        let mut lhs = self.binary(level + 1)?;
        while let Some(c) = self.eat_op(LEVELS[level]) {
            let rhs = self.binary(level + 1)?;
            let op = match c {
                '|' => Op::Or,
                '&' => Op::And,
                '+' => Op::Add,
                '-' => Op::Sub,
                '*' => Op::Mul,
                '/' => Op::Div,
                _ => Op::Mod,
            };
            lhs = Expr::Bin(op, Box::new(lhs), Box::new(rhs));
        }
        Ok(lhs)
    }

    fn unary(&mut self) -> Result<Expr> {
        if self.eat_op(&['-']).is_some() {
            return Ok(Expr::Neg(Box::new(self.unary()?)));
        }
        if self.eat_op(&['+']).is_some() {
            return self.unary();
        }
        self.primary()
    }

    fn primary(&mut self) -> Result<Expr> {
        match self.tokens.get(self.pos).cloned() {
            Some(Token::Num(v)) => {
                self.pos += 1;
                Ok(Expr::Num(v))
            }
            Some(Token::LParen) => {
                self.pos += 1;
                let e = self.expr()?;
                if !self.eat(&Token::RParen) {
                    bail!("expected ')'");
                }
                Ok(e)
            }
            Some(Token::Ident(name)) => {
                self.pos += 1;
                if !self.eat(&Token::LParen) {
                    return Ok(Expr::Var(self.ctx.slot(&name)));
                }
                let mut args = Vec::new();
                if !self.eat(&Token::RParen) {
                    loop {
                        args.push(self.expr()?);
                        if self.eat(&Token::RParen) {
                            break;
                        }
                        if !self.eat(&Token::Comma) {
                            bail!("expected ',' or ')' in call to {name}");
                        }
                    }
                }
                Ok(match (name.as_str(), args.len()) {
                    ("megabuf", 1) => Expr::Mem(Box::new(args.remove(0))),
                    ("gmegabuf", 1) => Expr::GMem(Box::new(args.remove(0))),
                    ("assign", 2) => {
                        let value = args.pop().unwrap_or(Expr::Num(0.0));
                        Expr::Assign(Box::new(args.remove(0)), Box::new(value))
                    }
                    _ => Expr::Call(name, args),
                })
            }
            other => bail!("unexpected token {other:?}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(src: &str, ctx: &mut Context) -> f64 {
        let mut gmem = Memory::new();
        Program::compile(src, ctx).unwrap().run(ctx, &mut gmem);
        ctx.get("out")
    }

    #[test]
    fn precedence_assignment_and_builtins() {
        let mut ctx = Context::default();
        assert_eq!(run("out = 1 + 2 * 3 - 8 / 4;", &mut ctx), 5.0);
        assert_eq!(run("out = -2 * -3", &mut ctx), 6.0);
        assert_eq!(run("a = b = 4; out = a + b;", &mut ctx), 8.0);
        assert_eq!(run("out = 7.5 % 2", &mut ctx), 1.0);
        assert_eq!(run("out = 1 & 0 | 1;", &mut ctx), 1.0);
        assert_eq!(run("out = if(above(3, 1), 10, 20)", &mut ctx), 10.0);
        assert_eq!(run("out = equal(0.1 + 0.2, 0.3) + bnot(0)", &mut ctx), 2.0);
        assert_eq!(
            run("out = int(2.7) + sqr(3) + max(1, 2) + sign(-4)", &mut ctx),
            12.0
        );
        assert_eq!(
            run("megabuf(5) = 42; out = megabuf(5) + megabuf(6)", &mut ctx),
            42.0
        );
        assert_eq!(run("n = 0; loop(4, n = n + 1); out = n", &mut ctx), 4.0);
        assert_eq!(
            run(
                "k = 0; while(exec2(k = k + 1, below(k, 3))); out = k",
                &mut ctx
            ),
            3.0
        );
        assert_eq!(
            run("Zoom = 1.5; out = ZOOM // comment\n + $pi*0", &mut ctx),
            1.5
        );
        let r = run("out = rand(10)", &mut ctx);
        assert!((0.0..10.0).contains(&r));
        assert!(Program::compile("out = 1 +", &mut ctx).is_err());
        assert!(Program::compile("", &mut ctx).unwrap().is_empty());
    }
}
