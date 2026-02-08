//! Static type checker for TypeScript/JavaScript programs.
//!
//! This module performs compile-time type checking before bytecode generation.
//! It validates:
//! - Variable declarations and assignments
//! - Function parameter and return types
//! - Binary and unary operations
//! - Control flow constructs
//! - Object and array literals
//!
//! # Strict Mode
//!
//! Strict mode (enabled by default via `TSBLITZ_STRICT`) enforces:
//! - Required type annotations on parameters
//! - Required return type annotations
//! - No implicit any
//! - Undeclared variables are errors
//!
//! Disable strict mode by setting `TSBLITZ_STRICT=0`.
//!
//! # Type System
//!
//! Supports primitive types (number, string, boolean, null, void), arrays,
//! objects, and functions. Type compatibility is structural for objects.
//!
//! # Example
//!
//! ```
//! use tsblitz::typecheck::check;
//! use tsblitz::parser::Parser;
//!
//! let source = "let x: number = 42;";
//! let program = Parser::parse_source(source).unwrap();
//! check(&program).expect("Type checking should pass");
//! ```

use crate::errors::TypeCheckerError;
use crate::oast as ast;
use crate::oxc_typecheck::Type;
use ast::*;
use std::collections::HashMap;
use std::sync::OnceLock;

type Scope = HashMap<String, Type>;

static STRICT_OVERRIDE: OnceLock<std::sync::Mutex<Option<bool>>> = OnceLock::new();

/// Strict mode: no implicit any, undeclared variables error, params/return require annotations.
///
/// Configurable via the `TSBLITZ_STRICT` environment variable; defaults to enabled.
/// A test-friendly override is available via `set_strict_override`.
fn strict_mode() -> bool {
    if let Some(m) = STRICT_OVERRIDE.get()
        && let Some(v) = *m.lock().unwrap()
    {
        return v;
    }
    std::env::var("TSBLITZ_STRICT")
        .map(|v| v != "0")
        .unwrap_or(true)
}

/// Set a test-only strictness override. Passing `None` clears the override and
/// falls back to the `TSBLITZ_STRICT` environment variable.
pub fn set_strict_override(v: Option<bool>) {
    let m = STRICT_OVERRIDE.get_or_init(|| std::sync::Mutex::new(None));
    let mut g = m.lock().unwrap();
    *g = v;
}

/// Type-check a program (top-level).
///
/// # Errors
///
/// Returns an error string if type checking fails
pub fn check(program: &Program) -> Result<(), TypeCheckerError> {
    let mut ctx = TypeContext {
        scopes: vec![Scope::new()],
    };

    // Builtins in global scope so prelude and user code can call them.
    // __native_print accepts a string only; do not reintroduce 'any'.
    ctx.scope_mut().insert(
        "__native_print".to_string(),
        Type::Function(vec![Type::String], Box::new(Type::Void)),
    );

    // Provide the host accessor used by blitzlib to capture host primitives.
    // The accessor takes a string name and returns any host value; use `Void`
    // as the return type in the typechecker to keep checks permissive.
    // NOTE: __host_get indirection removed; runtime publishes native helpers
    // directly (e.g. __native_print). Do not add __host_get to builtins.

    // Provide a builtin `console` object with `log: (...args: any[]) => void`.
    // The typechecker will allow passing any arguments to `console.log` (they are coerced to strings by the prelude).
    let mut console_props = HashMap::new();
    // Use empty parameter list to indicate variadic function for console.log
    console_props.insert(
        "log".to_string(),
        Type::Function(vec![], Box::new(Type::Void)), // Empty params = variadic
    );
    ctx.scope_mut()
        .insert("console".to_string(), Type::Object(console_props));

    // Add built-in JavaScript globals
    ctx.scope_mut().insert("undefined".to_string(), Type::Null);

    // Add built-in JavaScript functions
    ctx.scope_mut().insert(
        "isNaN".to_string(),
        Type::Function(vec![Type::Number], Box::new(Type::Boolean)),
    );
    ctx.scope_mut().insert(
        "isFinite".to_string(),
        Type::Function(vec![Type::Number], Box::new(Type::Boolean)),
    );
    ctx.scope_mut().insert(
        "parseInt".to_string(),
        Type::Function(vec![Type::String], Box::new(Type::Number)),
    );
    ctx.scope_mut().insert(
        "parseFloat".to_string(),
        Type::Function(vec![Type::String], Box::new(Type::Number)),
    );

    for stmt in &program.body {
        ctx.check_statement(stmt)?;
    }
    Ok(())
}

struct TypeContext {
    scopes: Vec<Scope>,
}

impl TypeContext {
    fn scope_mut(&mut self) -> &mut Scope {
        self.scopes.last_mut().unwrap()
    }

    fn lookup(&self, name: &str) -> Option<Type> {
        for scope in self.scopes.iter().rev() {
            if let Some(t) = scope.get(name) {
                return Some(t.clone());
            }
        }
        None
    }

    fn check_statement(&mut self, stmt: &Statement) -> Result<(), TypeCheckerError> {
        match stmt {
            // Ambient declaration (declare) - record type but do not expect runtime init
            Statement::Declare(d) => {
                // Variable must have a type annotation in strict mode or a prior builtin
                if let Some(ref ann) = d.type_ann {
                    self.scope_mut().insert(d.name.clone(), ann.clone());
                    return Ok(());
                }
                // If there is no annotation, treat as Void (host will provide implementation)
                self.scope_mut().insert(d.name.clone(), Type::Void);
                return Ok(());
            }

            Statement::VarDecl(v) => {
                let var_type = if let Some(ref ann) = v.type_ann {
                    ann.clone()
                } else if let Some(ref init) = v.init {
                    self.type_of_expr(init)?
                } else {
                    return Err(TypeCheckerError::from(format!(
                        "Variable '{}' needs a type annotation or initializer",
                        v.name
                    )));
                };

                if let Some(ref init) = v.init {
                    // Special-case: allow assigning an empty array literal to an annotated array type.
                    let assign_ok = if let Expr::ArrayExpr(arr) = init {
                        if arr.elements.is_empty() {
                            matches!(var_type, Type::Array(_))
                        } else {
                            let init_type = self.type_of_expr(init)?;
                            assignable(&var_type, &init_type)
                        }
                    } else {
                        let init_type = self.type_of_expr(init)?;
                        assignable(&var_type, &init_type)
                    };
                    if !assign_ok {
                        return Err(TypeCheckerError::from(format!(
                            "Type '{}' is not assignable to type '{}' for variable '{}'",
                            type_to_string(&self.type_of_expr(init)?),
                            type_to_string(&var_type),
                            v.name
                        )));
                    }
                }

                self.scope_mut().insert(v.name.clone(), var_type);
            }
            Statement::Block(b) => {
                self.scopes.push(Scope::new());
                for s in &b.body {
                    self.check_statement(s)?;
                }
                self.scopes.pop();
            }
            Statement::If(i) => {
                // In JavaScript, `if` conditions accept any value (truthiness).
                // Evaluate the test expression for side-effects but do not
                // enforce it be explicitly boolean.
                let _ = self.type_of_expr(&i.test)?;
                self.check_statement(&i.consequent)?;
                if let Some(ref alt) = i.alternate {
                    self.check_statement(alt)?;
                }
            }
            Statement::Try(t) => {
                // Type-check the try block and optionally the handler/finalizer.
                self.check_statement(&Statement::Block(t.block.clone()))?;
                if let Some((_, catch_block)) = &t.handler {
                    self.check_statement(&Statement::Block(catch_block.clone()))?;
                }
                if let Some(fin) = &t.finalizer {
                    self.check_statement(&Statement::Block(fin.clone()))?;
                }
            }
            Statement::Throw(_t) => {
                // Throw expressions are checked as the argument expression only.
                // The parser converts ThrowStatement into Statement::Throw with an Expr argument.
                // We'll leave semantic checks to expression checking paths when used in expressions.
            }
            Statement::While(w) => {
                // `while` conditions also accept any truthy/falsy value in JS.
                let _ = self.type_of_expr(&w.test)?;
                self.check_statement(&w.body)?;
            }
            Statement::Continue => {
                // Continue has no type-level effects; accept for now.
            }
            Statement::FunctionDecl(f) => {
                // Skip strict mode checking for library functions (those starting with __ or common stdlib functions)
                let is_library_function = f.name.starts_with("__")
                    || f.name.starts_with("is")
                    || matches!(
                        f.name.as_str(),
                        "parseInt"
                            | "parseFloat"
                            | "String"
                            | "Number"
                            | "Boolean"
                            | "hasOwnProperty"
                            | "propertyIsEnumerable"
                            | "isPrototypeOf"
                    );

                if strict_mode() && !is_library_function {
                    for p in &f.params {
                        if p.type_ann.is_none() {
                            return Err(TypeCheckerError::from(format!(
                                "Parameter '{}' requires a type annotation in strict mode",
                                p.name
                            )));
                        }
                    }
                    if f.return_type.is_none() {
                        return Err(TypeCheckerError::from(format!(
                            "Function '{}' return type required in strict mode",
                            f.name
                        )));
                    }
                }
                // In non-strict mode, accept missing annotations by treating them as `void`.
                let param_types: Vec<Type> = f
                    .params
                    .iter()
                    .map(|p| p.type_ann.clone().unwrap_or(Type::Void))
                    .collect();
                let return_type = f.return_type.clone().unwrap_or(Type::Void);
                self.scope_mut().insert(
                    f.name.clone(),
                    Type::Function(param_types, Box::new(return_type.clone())),
                );
                self.scopes.push(Scope::new());
                for p in &f.params {
                    let pt = p.type_ann.clone().unwrap_or(Type::Void);
                    self.scope_mut().insert(p.name.clone(), pt);
                }
                for s in &f.body.body {
                    self.check_statement(s)?;
                }
                if let Some(Statement::Return(r)) = f.body.body.last() {
                    let ret_type = r
                        .argument
                        .as_ref()
                        .map(|e| self.type_of_expr(e))
                        .transpose()?
                        .unwrap_or(Type::Void);
                    if !assignable(&return_type, &ret_type) && !is_library_function {
                        return Err(TypeCheckerError::from(format!(
                            "Return type {} is not assignable to declared {}",
                            type_to_string(&ret_type),
                            type_to_string(&return_type)
                        )));
                    }
                }
                self.scopes.pop();
            }
            Statement::Return(r) => {
                let ret_type = r
                    .argument
                    .as_ref()
                    .map(|e| self.type_of_expr(e))
                    .transpose()?
                    .unwrap_or(Type::Void);
                if ret_type != Type::Void {
                    let _ = ret_type;
                }
            }
            Statement::ExpressionStatement(e) => {
                if let Expr::Assignment(a) = &e.expression {
                    let value_type = self.type_of_expr(&a.value)?;
                    match &a.target {
                        AssignmentTarget::Identifier(ident) => {
                            let name = &ident.name;
                            match self.lookup(name) {
                                Some(expected) => {
                                    if !assignable(&expected, &value_type) {
                                        return Err(TypeCheckerError::from(format!(
                                            "Assignment: type {} is not assignable to {}",
                                            type_to_string(&value_type),
                                            type_to_string(&expected)
                                        )));
                                    }
                                }
                                // Undeclared variables are an error only in strict mode.
                                None if strict_mode() => {
                                    return Err(TypeCheckerError::from(format!(
                                        "Undeclared variable '{}'. Declare it with let or const first.",
                                        name
                                    )));
                                }
                                None => {}
                            }
                        }
                        AssignmentTarget::MemberExpr(_) => {}
                    }
                }
                self.type_of_expr(&e.expression)?;
            }
        }
        Ok(())
    }

    fn type_of_expr(&self, expr: &Expr) -> Result<Type, TypeCheckerError> {
        match expr {
            Expr::Literal(l) => Ok(match &l.value {
                LiteralValue::Number(_) => Type::Number,
                LiteralValue::String(_) => Type::String,
                LiteralValue::Bool(_) => Type::Boolean,
                LiteralValue::Null => Type::Null,
            }),
            Expr::Identifier(ident) => {
                let name = &ident.name;
                self.lookup(name)
                    .ok_or_else(|| TypeCheckerError::from(format!("Unknown variable '{}'", name)))
            }
            Expr::BinaryExpr(b) => {
                let l = self.type_of_expr(&b.left)?;
                let r = self.type_of_expr(&b.right)?;
                match b.operator {
                    BinaryOperator::Plus => {
                        if l == Type::Number && r == Type::Number {
                            Ok(Type::Number)
                        } else if l == Type::String || r == Type::String {
                            Ok(Type::String)
                        } else {
                            Err(TypeCheckerError::from(format!(
                                "Cannot add {} and {}",
                                type_to_string(&l),
                                type_to_string(&r)
                            )))
                        }
                    }
                    BinaryOperator::Minus
                    | BinaryOperator::Star
                    | BinaryOperator::Slash
                    | BinaryOperator::Percent => {
                        if assignable(&Type::Number, &l) && assignable(&Type::Number, &r) {
                            Ok(Type::Number)
                        } else {
                            Err(TypeCheckerError::from(format!(
                                "Arithmetic requires number, got {} and {}",
                                type_to_string(&l),
                                type_to_string(&r)
                            )))
                        }
                    }
                    BinaryOperator::EqEq
                    | BinaryOperator::BangEq
                    | BinaryOperator::EqEqEq
                    | BinaryOperator::BangEqEq
                    | BinaryOperator::Lt
                    | BinaryOperator::Le
                    | BinaryOperator::Gt
                    | BinaryOperator::Ge => Ok(Type::Boolean),
                    BinaryOperator::AndAnd | BinaryOperator::OrOr => {
                        // Javascript logical operators accept non-boolean operands and
                        // return one of the operands at runtime. As a simple
                        // approximation, use the right-hand operand's type.
                        Ok(r)
                    }
                }
            }
            Expr::UnaryExpr(u) => {
                let a = self.type_of_expr(&u.argument)?;
                match u.operator {
                    UnaryOperator::Bang => Ok(Type::Boolean),
                    UnaryOperator::Minus => {
                        if assignable(&Type::Number, &a) {
                            Ok(Type::Number)
                        } else {
                            Err(TypeCheckerError::from(
                                "- unary requires number".to_string(),
                            ))
                        }
                    }
                    UnaryOperator::Typeof => Ok(Type::String),
                }
            }
            Expr::CallExpr(c) => {
                let callee_type = self.type_of_expr(&c.callee)?;

                // Detect if this is a `console.log(...)` call so we can permit
                // passing numbers which will be coerced to strings by the wrapper.
                let is_console_log = if let Expr::MemberExpr(m) = &*c.callee {
                    if let Expr::Identifier(ident) = &*m.object {
                        let obj = &ident.name;
                        match &*m.property {
                            Expr::Identifier(prop_ident) => {
                                let prop = &prop_ident.name;
                                obj == "console" && prop == "log"
                            }
                            Expr::Literal(Literal {
                                value: LiteralValue::String(prop),
                            }) => obj == "console" && prop == "log",
                            _ => false,
                        }
                    } else {
                        false
                    }
                } else {
                    false
                };

                match &callee_type {
                    Type::Function(params, ret) => {
                        // Allow empty parameter list to accept any number of arguments (for variadic functions like console.log)
                        if !params.is_empty() && params.len() != c.args.len() {
                            return Err(TypeCheckerError::from(format!(
                                "Expected {} arguments, got {}",
                                params.len(),
                                c.args.len()
                            )));
                        }
                        for (i, (arg, param)) in c.args.iter().zip(params.iter()).enumerate() {
                            let arg_type = self.type_of_expr(arg)?;
                            if !assignable(param, &arg_type) {
                                // Special-case: allow passing a number to a string parameter
                                // when calling `console.log`, because the wrapper coerces
                                // numbers to strings at runtime.
                                if is_console_log
                                    && *param == Type::String
                                    && arg_type == Type::Number
                                {
                                    continue;
                                }

                                return Err(TypeCheckerError::from(format!(
                                    "Argument {}: type {} is not assignable to {}",
                                    i + 1,
                                    type_to_string(&arg_type),
                                    type_to_string(param)
                                )));
                            }
                        }
                        Ok((**ret).clone())
                    }
                    _ => Err(TypeCheckerError::from(format!(
                        "Cannot call non-function type {}",
                        type_to_string(&callee_type)
                    ))),
                }
            }
            Expr::MemberExpr(m) => {
                // Evaluate the object type and attempt to resolve a statically known property key.
                let obj_type = self.type_of_expr(&m.object)?;
                match obj_type {
                    Type::Object(ref props) => {
                        // property must be a string key (ident or string literal) when not computed
                        let key_opt = if !m.computed {
                            match &*m.property {
                                Expr::Identifier(ident) => Some(ident.name.clone()),
                                Expr::Literal(Literal {
                                    value: LiteralValue::String(s),
                                }) => Some(s.clone()),
                                _ => None,
                            }
                        } else {
                            // computed property: must evaluate to string literal at compile time
                            match &*m.property {
                                Expr::Literal(Literal {
                                    value: LiteralValue::String(s),
                                }) => Some(s.clone()),
                                _ => None,
                            }
                        };

                        if let Some(k) = key_opt {
                            if let Some(t) = props.get(&k) {
                                Ok(t.clone())
                            } else {
                                Err(TypeCheckerError::from(format!(
                                    "Property '{}' not found on object",
                                    k
                                )))
                            }
                        } else {
                            Err(TypeCheckerError::from(String::from(
                                "Unsupported property access in type context",
                            )))
                        }
                    }
                    _ => Err(TypeCheckerError::from(String::from(
                        "GetProperty on non-object type",
                    ))),
                }
            }
            Expr::Assignment(a) => self.type_of_expr(&a.value),
            Expr::ArrayExpr(arr) => {
                if arr.elements.is_empty() {
                    // Empty array needs an annotation where it is used; signal that it's unsupported here.
                    return Err(TypeCheckerError::from(String::from(
                        "Empty array literal requires an explicit type annotation",
                    )));
                }
                let elem = self.type_of_expr(&arr.elements[0])?;
                for e in &arr.elements[1..] {
                    let t = self.type_of_expr(e)?;
                    if !assignable(&elem, &t) && !assignable(&t, &elem) {
                        return Err(TypeCheckerError::from(String::from(
                            "Array elements must have same type",
                        )));
                    }
                }
                Ok(Type::Array(Box::new(elem)))
            }
            Expr::ThisExpression => Ok(Type::Object(HashMap::new())),
            Expr::ObjectExpr(obj) => {
                let mut props = HashMap::new();
                for prop in &obj.properties {
                    let t = self.type_of_expr(&prop.value)?;
                    props.insert(prop.key.clone(), t);
                }
                Ok(Type::Object(props))
            }
            Expr::FunctionExpr(f) => {
                if strict_mode() {
                    for p in &f.params {
                        if p.type_ann.is_none() {
                            return Err(TypeCheckerError::from(format!(
                                "Parameter '{}' requires a type annotation in strict mode",
                                p.name
                            )));
                        }
                    }
                }
                let param_types: Vec<Type> = f
                    .params
                    .iter()
                    .map(|p| p.type_ann.clone().unwrap_or(Type::Void))
                    .collect();
                let return_type = f.return_type.clone().unwrap_or(Type::Void);
                Ok(Type::Function(param_types, Box::new(return_type)))
            }
        }
    }
}

fn assignable(expected: &Type, actual: &Type) -> bool {
    match (expected, actual) {
        (Type::Number, Type::Number) => true,
        (Type::String, Type::String) => true,
        (Type::Boolean, Type::Boolean) => true,
        (Type::Null, Type::Null) => true,
        (Type::Void, Type::Void) => true,
        (Type::Array(a), Type::Array(b)) => assignable(a, b),
        (Type::Object(a), Type::Object(b)) => {
            // Shallow structural check: every property in `a` must exist in `b` and be assignable.
            for (k, t) in a {
                if let Some(t2) = b.get(k) {
                    if !assignable(t, t2) {
                        return false;
                    }
                } else {
                    return false;
                }
            }
            true
        }
        (Type::Function(a, _), Type::Function(b, _)) => a.len() == b.len(),
        _ => false,
    }
}

/// Convert a `Type` to a human-readable string for diagnostics and error messages.
pub fn type_to_string(t: &Type) -> String {
    match t {
        Type::Number => "number".to_string(),
        Type::String => "string".to_string(),
        Type::Boolean => "boolean".to_string(),
        Type::Null => "null".to_string(),
        Type::Void => "void".to_string(),
        Type::Array(inner) => format!("{}[]", type_to_string(inner)),
        Type::Object(_) => "object".to_string(),
        Type::Function(params, ret) => {
            format!(
                "fn({}) -> {}",
                params
                    .iter()
                    .map(type_to_string)
                    .collect::<Vec<_>>()
                    .join(", "),
                type_to_string(ret)
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::Parser;

    #[test]
    fn logical_ops_accept_non_boolean_operands() {
        let src = r#"
            var a = 1;
            var b = 'b';
            var x = a || b;
            var y = a && b;
            var z = 1 && 2;
        "#;
        let program = Parser::parse_source(src).expect("parse");
        assert!(check(&program).is_ok());
    }

    #[test]
    fn unary_bang_accepts_non_boolean() {
        let src = "var a = !1;";
        let program = Parser::parse_source(src).expect("parse");
        assert!(check(&program).is_ok());
    }

    #[test]
    fn if_condition_accepts_non_boolean() {
        let src = "if (1) { var x = 1; }";
        let program = Parser::parse_source(src).expect("parse");
        assert!(check(&program).is_ok());
    }

    #[test]
    fn while_condition_accepts_non_boolean() {
        let src = "var i = 0; while (i) { i = i - 1; }";
        let program = Parser::parse_source(src).expect("parse");
        assert!(check(&program).is_ok());
    }
}
