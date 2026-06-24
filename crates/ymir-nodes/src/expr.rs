//! A tiny arithmetic-expression engine for the Expression node.
//!
//! An expression is parsed once into a flat bytecode program, then evaluated per cell
//! against a fixed variable environment. The per-cell hot path is a stack machine over a
//! `Vec<Op>` (no AST pointer-chasing, no per-cell allocation), so it stays fast over
//! millions of cells and parallelizes trivially: [`Program::eval`] is `&self` plus a
//! local stack, so each rayon worker evaluates independently.
//!
//! It is an expression language, not a script: no statements, control flow, or
//! assignment. Branching is done with `select(cond, a, b)` and `clamp`. Identifiers
//! resolve to the caller's variables (the input layers and cell coordinates) or to the
//! built-in constants and functions; an unknown name is a compile error, so a typo is
//! reported rather than silently zero.
//!
//! Hand-rolled rather than pulling an eval crate, for the same reason `noise.rs` is: the
//! function set and numeric behavior are fully under our control and byte-stable, and the
//! fast crate (`fasteval`) would be a dependency with the same version liability while the
//! easy crates (`evalexpr`, `rhai`) are tree-walkers, the slow per-cell path.

/// Largest evaluation-stack depth a program may need. Real expressions are far shallower;
/// the cap bounds the per-cell stack array and is checked once at compile time.
const STACK_MAX: usize = 128;

/// A built-in function, with a fixed arity.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Func {
    // One argument.
    Sin,
    Cos,
    Tan,
    Abs,
    Sqrt,
    Floor,
    Ceil,
    Exp,
    Ln,
    Sign,
    // Two arguments.
    Min,
    Max,
    Pow,
    Atan2,
    Step,
    // Three arguments.
    Clamp,
    Lerp,
    Smoothstep,
    Select,
}

impl Func {
    /// The function name as written in an expression, or `None` if `name` is not a
    /// built-in.
    fn from_name(name: &str) -> Option<Self> {
        Some(match name {
            "sin" => Self::Sin,
            "cos" => Self::Cos,
            "tan" => Self::Tan,
            "abs" => Self::Abs,
            "sqrt" => Self::Sqrt,
            "floor" => Self::Floor,
            "ceil" => Self::Ceil,
            "exp" => Self::Exp,
            "ln" => Self::Ln,
            "sign" => Self::Sign,
            "min" => Self::Min,
            "max" => Self::Max,
            "pow" => Self::Pow,
            "atan2" => Self::Atan2,
            "step" => Self::Step,
            "clamp" => Self::Clamp,
            "lerp" => Self::Lerp,
            "smoothstep" => Self::Smoothstep,
            "select" => Self::Select,
            _ => return None,
        })
    }

    /// How many arguments the function takes.
    fn arity(self) -> usize {
        match self {
            Self::Sin
            | Self::Cos
            | Self::Tan
            | Self::Abs
            | Self::Sqrt
            | Self::Floor
            | Self::Ceil
            | Self::Exp
            | Self::Ln
            | Self::Sign => 1,
            Self::Min | Self::Max | Self::Pow | Self::Atan2 | Self::Step => 2,
            Self::Clamp | Self::Lerp | Self::Smoothstep | Self::Select => 3,
        }
    }

    /// Applies the function to its argument slice (length equals [`arity`](Self::arity)).
    fn apply(self, a: &[f32]) -> f32 {
        match self {
            Self::Sin => a[0].sin(),
            Self::Cos => a[0].cos(),
            Self::Tan => a[0].tan(),
            Self::Abs => a[0].abs(),
            Self::Sqrt => a[0].sqrt(),
            Self::Floor => a[0].floor(),
            Self::Ceil => a[0].ceil(),
            Self::Exp => a[0].exp(),
            Self::Ln => a[0].ln(),
            Self::Sign => a[0].signum(),
            Self::Min => a[0].min(a[1]),
            Self::Max => a[0].max(a[1]),
            Self::Pow => a[0].powf(a[1]),
            Self::Atan2 => a[0].atan2(a[1]),
            // step(edge, x): 0 below the edge, 1 at or above it.
            Self::Step => {
                if a[1] < a[0] {
                    0.0
                } else {
                    1.0
                }
            }
            Self::Clamp => a[0].clamp(a[1], a[2]),
            Self::Lerp => a[0] + (a[1] - a[0]) * a[2],
            // smoothstep(e0, e1, x): Hermite ease from 0 to 1 across [e0, e1].
            Self::Smoothstep => {
                let span = a[1] - a[0];
                let t = if span.abs() < f32::EPSILON {
                    f32::from(a[2] >= a[1])
                } else {
                    ((a[2] - a[0]) / span).clamp(0.0, 1.0)
                };
                t * t * (3.0 - 2.0 * t)
            }
            // select(cond, a, b): a when cond is non-zero, else b.
            Self::Select => {
                if a[0] != 0.0 {
                    a[1]
                } else {
                    a[2]
                }
            }
        }
    }
}

/// A named numeric constant usable in an expression (resolved before variables).
fn constant(name: &str) -> Option<f32> {
    Some(match name {
        "pi" => std::f32::consts::PI,
        "tau" => std::f32::consts::TAU,
        "e" => std::f32::consts::E,
        _ => return None,
    })
}

/// One bytecode instruction of a compiled expression.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Op {
    /// Push a literal.
    Const(f32),
    /// Push the value of variable `index` from the eval environment.
    Var(u16),
    /// Negate the top of the stack.
    Neg,
    /// Pop two, push their sum / difference / product / quotient / power.
    Add,
    Sub,
    Mul,
    Div,
    Pow,
    /// Pop the function's arguments, push its result.
    Func(Func),
}

/// A compile/parse failure, carrying a human-readable message for the node to surface.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ExprError {
    /// The message, e.g. `unknown name "heigth"` or `expected ')'`.
    pub message: String,
}

impl ExprError {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl std::fmt::Display for ExprError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

/// A compiled expression, evaluated per cell. Compile once, evaluate many.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Program {
    ops: Vec<Op>,
}

impl Program {
    /// Compiles `source`, resolving identifiers against `vars` (in order) plus the
    /// built-in constants and functions.
    ///
    /// # Errors
    ///
    /// Returns an [`ExprError`] with a message on a syntax error, an unknown name, a
    /// function-arity mismatch, or an expression too deep to evaluate.
    pub(crate) fn compile(source: &str, vars: &[&str]) -> Result<Self, ExprError> {
        let tokens = tokenize(source)?;
        let mut parser = Parser {
            tokens,
            pos: 0,
            vars,
            ops: Vec::new(),
            depth: 0,
            max_depth: 0,
        };
        parser.parse_expr(0)?;
        if parser.peek() != &Token::End {
            return Err(ExprError::new("unexpected trailing input"));
        }
        if parser.max_depth > STACK_MAX {
            return Err(ExprError::new("expression is too deeply nested"));
        }
        Ok(Self { ops: parser.ops })
    }

    /// Evaluates the program with `values[i]` bound to the `i`th variable from
    /// [`compile`](Self::compile). The caller guarantees `values.len()` covers every
    /// variable index the program references.
    pub(crate) fn eval(&self, values: &[f32]) -> f32 {
        // A fixed stack array, so the per-cell hot path never allocates. The compile-time
        // depth check guarantees the indices below stay in bounds.
        let mut stack = [0.0_f32; STACK_MAX];
        let mut sp = 0usize;
        for op in &self.ops {
            match op {
                Op::Const(c) => {
                    stack[sp] = *c;
                    sp += 1;
                }
                Op::Var(i) => {
                    stack[sp] = values[*i as usize];
                    sp += 1;
                }
                Op::Neg => stack[sp - 1] = -stack[sp - 1],
                Op::Add => {
                    sp -= 1;
                    stack[sp - 1] += stack[sp];
                }
                Op::Sub => {
                    sp -= 1;
                    stack[sp - 1] -= stack[sp];
                }
                Op::Mul => {
                    sp -= 1;
                    stack[sp - 1] *= stack[sp];
                }
                Op::Div => {
                    sp -= 1;
                    stack[sp - 1] /= stack[sp];
                }
                Op::Pow => {
                    sp -= 1;
                    stack[sp - 1] = stack[sp - 1].powf(stack[sp]);
                }
                Op::Func(f) => {
                    let n = f.arity();
                    sp -= n;
                    stack[sp] = f.apply(&stack[sp..sp + n]);
                    sp += 1;
                }
            }
        }
        stack[0]
    }
}

/// A lexical token.
#[derive(Clone, Debug, PartialEq)]
enum Token {
    Number(f64),
    Ident(String),
    Plus,
    Minus,
    Star,
    Slash,
    Caret,
    LParen,
    RParen,
    Comma,
    End,
}

/// Splits `source` into tokens, erroring on an unrecognized character or a malformed
/// number.
fn tokenize(source: &str) -> Result<Vec<Token>, ExprError> {
    let mut tokens = Vec::new();
    let chars: Vec<char> = source.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        match c {
            c if c.is_whitespace() => i += 1,
            '+' => {
                tokens.push(Token::Plus);
                i += 1;
            }
            '-' => {
                tokens.push(Token::Minus);
                i += 1;
            }
            '*' => {
                tokens.push(Token::Star);
                i += 1;
            }
            '/' => {
                tokens.push(Token::Slash);
                i += 1;
            }
            '^' => {
                tokens.push(Token::Caret);
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
            c if c.is_ascii_digit() || c == '.' => {
                let start = i;
                while i < chars.len() && (chars[i].is_ascii_digit() || chars[i] == '.') {
                    i += 1;
                }
                // Optional exponent: e / E, an optional sign, then digits.
                if i < chars.len() && (chars[i] == 'e' || chars[i] == 'E') {
                    i += 1;
                    if i < chars.len() && (chars[i] == '+' || chars[i] == '-') {
                        i += 1;
                    }
                    while i < chars.len() && chars[i].is_ascii_digit() {
                        i += 1;
                    }
                }
                let text: String = chars[start..i].iter().collect();
                let value = text
                    .parse::<f64>()
                    .map_err(|_| ExprError::new(format!("invalid number \"{text}\"")))?;
                tokens.push(Token::Number(value));
            }
            c if c.is_alphabetic() || c == '_' => {
                let start = i;
                while i < chars.len() && (chars[i].is_alphanumeric() || chars[i] == '_') {
                    i += 1;
                }
                tokens.push(Token::Ident(chars[start..i].iter().collect()));
            }
            other => return Err(ExprError::new(format!("unexpected character '{other}'"))),
        }
    }
    tokens.push(Token::End);
    Ok(tokens)
}

/// A precedence-climbing (Pratt) parser that emits bytecode directly.
struct Parser<'a> {
    tokens: Vec<Token>,
    pos: usize,
    vars: &'a [&'a str],
    ops: Vec<Op>,
    /// Current evaluation-stack depth as ops are emitted, and the peak, so the compiler
    /// can reject a program that would overflow the eval stack.
    depth: usize,
    max_depth: usize,
}

impl Parser<'_> {
    fn peek(&self) -> &Token {
        &self.tokens[self.pos]
    }

    fn advance(&mut self) -> Token {
        let token = self.tokens[self.pos].clone();
        self.pos += 1;
        token
    }

    /// Emits an op and tracks the stack depth it leaves behind.
    fn emit(&mut self, op: Op) {
        match op {
            Op::Const(_) | Op::Var(_) => self.depth += 1,
            Op::Neg => {}
            Op::Add | Op::Sub | Op::Mul | Op::Div | Op::Pow => self.depth -= 1,
            Op::Func(f) => self.depth -= f.arity() - 1,
        }
        self.max_depth = self.max_depth.max(self.depth);
        self.ops.push(op);
    }

    /// Parses an expression whose operators bind at least as tightly as `min_bp`.
    fn parse_expr(&mut self, min_bp: u8) -> Result<(), ExprError> {
        self.parse_prefix()?;
        loop {
            // (left binding power, right binding power) per infix operator. Equal-or-
            // greater right power than left is right-associative (only `^`).
            let (op, l_bp, r_bp) = match self.peek() {
                Token::Plus => (Op::Add, 1, 2),
                Token::Minus => (Op::Sub, 1, 2),
                Token::Star => (Op::Mul, 3, 4),
                Token::Slash => (Op::Div, 3, 4),
                Token::Caret => (Op::Pow, 5, 5),
                _ => break,
            };
            if l_bp < min_bp {
                break;
            }
            self.advance();
            self.parse_expr(r_bp)?;
            self.emit(op);
        }
        Ok(())
    }

    /// Parses a prefix term: a number, a parenthesized expression, a unary +/-, a
    /// constant, a variable, or a function call.
    fn parse_prefix(&mut self) -> Result<(), ExprError> {
        match self.advance() {
            Token::Number(n) => self.emit(Op::Const(n as f32)),
            // Unary minus binds tighter than +,-,*,/ but looser than ^, so -2^2 = -(2^2).
            Token::Minus => {
                self.parse_expr(4)?;
                self.emit(Op::Neg);
            }
            Token::Plus => self.parse_expr(4)?,
            Token::LParen => {
                self.parse_expr(0)?;
                self.consume(&Token::RParen)?;
            }
            Token::Ident(name) => self.parse_ident(name)?,
            other => return Err(ExprError::new(format!("unexpected token {other:?}"))),
        }
        Ok(())
    }

    /// Parses an identifier term: a function call `name(args)`, a named constant, or a
    /// variable.
    fn parse_ident(&mut self, name: String) -> Result<(), ExprError> {
        if self.peek() == &Token::LParen {
            self.advance();
            let func = Func::from_name(&name)
                .ok_or_else(|| ExprError::new(format!("unknown function \"{name}\"")))?;
            let mut argc = 0;
            if self.peek() != &Token::RParen {
                loop {
                    self.parse_expr(0)?;
                    argc += 1;
                    if self.peek() == &Token::Comma {
                        self.advance();
                    } else {
                        break;
                    }
                }
            }
            self.consume(&Token::RParen)?;
            if argc != func.arity() {
                return Err(ExprError::new(format!(
                    "{name} takes {} argument(s), got {argc}",
                    func.arity()
                )));
            }
            self.emit(Op::Func(func));
        } else if let Some(c) = constant(&name) {
            self.emit(Op::Const(c));
        } else if let Some(index) = self.vars.iter().position(|v| *v == name) {
            self.emit(Op::Var(index as u16));
        } else {
            return Err(ExprError::new(format!("unknown name \"{name}\"")));
        }
        Ok(())
    }

    /// Advances past `token`, or errors if the next token is something else.
    fn consume(&mut self, token: &Token) -> Result<(), ExprError> {
        if self.peek() == token {
            self.advance();
            Ok(())
        } else {
            Err(ExprError::new(format!(
                "expected {token:?}, found {:?}",
                self.peek()
            )))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Compiles and evaluates `source` against `vars`/`values`, panicking on a compile
    /// error (the test asserts the value).
    fn eval(source: &str, vars: &[&str], values: &[f32]) -> f32 {
        Program::compile(source, vars)
            .expect("compiles")
            .eval(values)
    }

    fn eval0(source: &str) -> f32 {
        eval(source, &[], &[])
    }

    #[test]
    fn arithmetic_and_precedence() {
        assert_eq!(eval0("1 + 2 * 3"), 7.0);
        assert_eq!(eval0("(1 + 2) * 3"), 9.0);
        assert_eq!(eval0("10 - 2 - 3"), 5.0); // left-associative
        assert_eq!(eval0("8 / 4 / 2"), 1.0); // left-associative
        assert_eq!(eval0("2 ^ 3 ^ 2"), 512.0); // right-associative: 2^(3^2)
    }

    #[test]
    fn unary_minus_binds_below_pow() {
        assert_eq!(eval0("-2 ^ 2"), -4.0); // -(2^2)
        assert_eq!(eval0("-2 * 3"), -6.0);
        assert_eq!(eval0("- -5"), 5.0);
    }

    #[test]
    fn variables_resolve_by_position() {
        assert_eq!(eval("h * 2 + x", &["h", "x"], &[0.25, 0.5]), 1.0);
    }

    #[test]
    fn constants_and_functions() {
        assert!((eval0("sin(0)")).abs() < 1e-6);
        assert!((eval0("cos(0)") - 1.0).abs() < 1e-6);
        assert_eq!(eval0("clamp(5, 0, 1)"), 1.0);
        assert_eq!(eval0("clamp(-5, 0, 1)"), 0.0);
        assert_eq!(eval0("lerp(0, 10, 0.5)"), 5.0);
        assert_eq!(eval0("min(3, 7)"), 3.0);
        assert_eq!(eval0("max(3, 7)"), 7.0);
        assert_eq!(eval0("step(0.5, 0.4)"), 0.0);
        assert_eq!(eval0("step(0.5, 0.6)"), 1.0);
        assert_eq!(eval0("select(1, 2, 3)"), 2.0);
        assert_eq!(eval0("select(0, 2, 3)"), 3.0);
        assert!((eval0("pi") - std::f32::consts::PI).abs() < 1e-6);
    }

    #[test]
    fn smoothstep_eases() {
        assert_eq!(eval0("smoothstep(0, 1, -1)"), 0.0);
        assert_eq!(eval0("smoothstep(0, 1, 2)"), 1.0);
        assert!((eval0("smoothstep(0, 1, 0.5)") - 0.5).abs() < 1e-6);
    }

    #[test]
    fn unknown_name_is_an_error() {
        let err = Program::compile("heigth + 1", &["height"]).unwrap_err();
        assert!(err.message.contains("heigth"));
    }

    #[test]
    fn unknown_function_is_an_error() {
        assert!(Program::compile("wobble(1)", &[]).is_err());
    }

    #[test]
    fn wrong_arity_is_an_error() {
        let err = Program::compile("clamp(1, 2)", &[]).unwrap_err();
        assert!(err.message.contains("argument"));
    }

    #[test]
    fn syntax_errors_are_reported() {
        assert!(Program::compile("1 +", &[]).is_err());
        assert!(Program::compile("(1 + 2", &[]).is_err());
        assert!(Program::compile("1 2", &[]).is_err());
        assert!(Program::compile("", &[]).is_err());
    }

    #[test]
    fn eval_is_deterministic_and_reusable() {
        let program = Program::compile("sin(x * 6.2831) * h", &["x", "h"]).unwrap();
        let a = program.eval(&[0.3, 0.7]);
        let b = program.eval(&[0.3, 0.7]);
        assert_eq!(a, b);
        // Different inputs give different output.
        assert_ne!(program.eval(&[0.3, 0.7]), program.eval(&[0.4, 0.7]));
    }
}
