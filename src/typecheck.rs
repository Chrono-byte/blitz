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
//! The checker consumes Oxc's `oast::Program` directly (no ESTree/owned-ast
//! round-trip). It provides a lightweight conservative `Type` model used for
//! soft-layer and prelude validation.

use crate::errors::TypeCheckerError;
use oxc_ast::ast as oast;

// Reintroduce a compact Type enum here (previously in oxc_typecheck) so other
// modules can reference `crate::typecheck::Type` after removing the duplicate
// module. Keep it conservative and compatible with existing usage.
use std::collections::HashMap;
use std::sync::OnceLock;

/// Lightweight, conservative representation of types used by the checker.
///
/// This `Type` enum is not a full TypeScript type system; it models the
/// subset of types that the VM and the soft layer (blitzlib) need for
/// basic static checks. It is intentionally small and pragmatic:
///
/// - Primitive kinds: `Number`, `String`, `Boolean`, `Null`, `Void`.
/// - Top-like/unknown markers: `Any`, `Unknown`
/// - Compound shapes: `Array`, `Function`, `Object`
/// - `Builtin` enumerates host-provided runtime categories (see `BuiltinType`)
/// - `TypeReference` names a type that should be resolved via lexical scope.
/// - `Union` models simple union types (no type algebra beyond membership).
///
/// Notes on usage and semantics:
/// - `Type::Array(inner)` denotes a homogeneous array of `inner` type.
/// - `Type::Function(params, ret)` uses a `Vec<Type>` for parameters and a boxed return type.
///   We treat empty parameter lists as a convention to mean "variadic" (e.g. `console.log` in the prelude).
/// - `Type::Object` performs shallow structural checks in `assignable`:
///   every property declared in the expected object must exist and be assignable.
/// - `TypeReference` is a name-only reference; callers must resolve it via the checker `Scope`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Type {
    /// Numeric values (JS `number`).
    Number,
    /// String values (JS `string`).
    String,
    /// Boolean values (JS `boolean`).
    Boolean,
    /// The `null` literal/marker.
    Null,
    /// Represents `void`/no-value (used for function returns and some host APIs).
    Void,
    /// A deliberately permissive top type. Used when the caller chooses to be lax.
    Any,
    /// An unknown type returned from incomplete inference; treated separately from `Any`.
    Unknown,
    /// Homogeneous array type.
    Array(Box<Type>),
    /// Function type with parameter types and return type.
    Function(Vec<Type>, Box<Type>),
    /// Object with named properties and their types.
    Object(HashMap<String, Type>),
    /// A builtin runtime category.
    Builtin(BuiltinType),
    /// A reference to a named type that should be looked up in the current scope.
    TypeReference {
        /// Name of the referred type.
        name: String,
    },
    /// Union of other `Type` variants (simple, flat unions).
    Union(Vec<Type>),
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
/// Enumeration of runtime built-in type categories that the checker recognizes.
///
/// These correspond to the engine's coarse-grained runtime kinds. The
/// typechecker uses `BuiltinType` when representing host-provided values
/// and to format diagnostics that mention runtime types.
///
/// Note: this is not an extensible tag-scheme; if you add runtime categories
/// you should update both this enum and call sites that map runtime values to
/// builtins.
pub enum BuiltinType {
    /// Represents `undefined` values.
    Undefined,
    /// Represents `null`.
    Null,
    /// Boolean runtime kind.
    Boolean,
    /// Numeric runtime kind.
    Number,
    /// String runtime kind.
    String,
    /// Symbol runtime kind.
    Symbol,
    /// BigInt runtime kind.
    BigInt,
    /// Generic object runtime kind.
    Object,
    /// Function runtime kind.
    Function,
    /// Array runtime kind.
    Array,
}

type Scope = HashMap<String, Type>;

static STRICT_OVERRIDE: OnceLock<std::sync::Mutex<Option<bool>>> = OnceLock::new();

/// Return whether strict mode is currently enabled.
///
/// The checker consults this flag to decide whether to require parameter and
/// return annotations, to reject undeclared variables, and to disallow implicit
/// `any`. The effective strictness is determined by, in order:
/// 1. The test-only override set via `set_strict_override(Some(bool))`.
/// 2. The environment variable `TSBLITZ_STRICT` (non-zero means enabled).
/// 3. Defaults to enabled when the environment variable is not set.
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

pub fn set_strict_override(v: Option<bool>) {
    let m = STRICT_OVERRIDE.get_or_init(|| std::sync::Mutex::new(None));
    let mut g = m.lock().unwrap();
    *g = v;
}

pub fn check(program: &oast::Program<'_>) -> Result<(), TypeCheckerError> {
    let mut ctx = TypeContext {
        scopes: vec![Scope::new()],
    };

    // Builtins
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
        Type::Function(vec![], Box::new(Type::Void)),
    ); // variadic
    ctx.scope_mut()
        .insert("console".to_string(), Type::Object(console_props));
    ctx.scope_mut().insert("undefined".to_string(), Type::Null);
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

    for item in &program.body {
        if let oast::ModuleItem::Stmt(stmt) = item {
            ctx.check_statement_oast(stmt)?;
        }
    }
    Ok(())
}

/// Compatibility entry: kept for callers that still call `check_oxc`.
pub fn check_oxc(program: &oast::Program<'_>) -> Result<(), TypeCheckerError> {
    check(program)
}

struct TypeContext {
    scopes: Vec<Scope>,
}

impl TypeContext {
    fn scope_mut(&mut self) -> &mut Scope {
        self.scopes.last_mut().unwrap()
    }

    fn lookup(&self, name: &str) -> Option<Type> {
        for s in self.scopes.iter().rev() {
            if let Some(t) = s.get(name) {
                return Some(t.clone());
            }
        }
        None
    }

    fn check_statement_oast(&mut self, stmt: &oast::Stmt) -> Result<(), TypeCheckerError> {
        use oast::{Decl as ODecl, Pat as OPat, Stmt as OStmt};
        match stmt {
            OStmt::Decl(d) => match d {
                ODecl::Var(v) => {
                    for dec in &v.decls {
                        let name = match &dec.id {
                            OPat::Ident(i) => i.id.name.clone(),
                            _ => return Err(TypeCheckerError::from("unsupported var pattern")),
                        };
                        let var_type = if let Some(typ) = &dec.ty {
                            convert_type_ann(typ)?
                        } else if let Some(init) = &dec.init {
                            self.type_of_expr_oast(init)?
                        } else {
                            return Err(TypeCheckerError::from(format!(
                                "Variable '{}' needs a type annotation or initializer",
                                name
                            )));
                        };
                        if let Some(init) = &dec.init {
                            let init_ok = if let oast::Expr::Array(arr) = init {
                                if arr.elements.is_empty() {
                                    matches!(var_type, Type::Array(_))
                                } else {
                                    let it = self.type_of_expr_oast(init)?;
                                    assignable(&var_type, &it)
                                }
                            } else {
                                let it = self.type_of_expr_oast(init)?;
                                assignable(&var_type, &it)
                            };
                            if !init_ok {
                                return Err(TypeCheckerError::from(format!(
                                    "Type '{}' is not assignable to '{}' for variable '{}'",
                                    type_to_string(&self.type_of_expr_oast(init)?),
                                    type_to_string(&var_type),
                                    name
                                )));
                            }
                        }
                        self.scope_mut().insert(name, var_type);
                    }
                }
                ODecl::Fn(f) => {
                    if let Some(ident) = &f.ident {
                        let fname = ident.name.clone();
                        // parameters and return types live on `oast::Function` as `ty`/`return_type` optionally
                        let mut param_types = Vec::new();
                        for p in &f.params {
                            if strict_mode() && p.ty.is_none() {
                                return Err(TypeCheckerError::from(format!(
                                    "Parameter '{}' requires a type annotation in strict mode",
                                    match &p.pat {
                                        OPat::Ident(i) => i.id.name.clone(),
                                        _ => "<pat>".to_string(),
                                    }
                                )));
                            }
                            param_types.push(
                                p.ty.as_ref()
                                    .map(|t| convert_type_ann(t))
                                    .transpose()?
                                    .unwrap_or(Type::Void),
                            );
                        }
                        if strict_mode() && f.return_type.is_none() {
                            return Err(TypeCheckerError::from(format!(
                                "Function '{}' return type required in strict mode",
                                fname
                            )));
                        }
                        let return_type = f
                            .return_type
                            .as_ref()
                            .map(|t| convert_type_ann(t))
                            .transpose()?
                            .unwrap_or(Type::Void);
                        self.scope_mut().insert(
                            fname.clone(),
                            Type::Function(param_types, Box::new(return_type.clone())),
                        );
                        // check body
                        if let Some(body) = &f.body {
                            self.scopes.push(Scope::new());
                            for p in &f.params {
                                let pname = match &p.pat {
                                    OPat::Ident(i) => i.id.name.clone(),
                                    _ => "_".to_string(),
                                };
                                let pt =
                                    p.ty.as_ref()
                                        .map(|t| convert_type_ann(t))
                                        .transpose()?
                                        .unwrap_or(Type::Void);
                                self.scope_mut().insert(pname, pt);
                            }
                            for s in &body.body {
                                self.check_statement_oast(s)?;
                            }
                            self.scopes.pop();
                        }
                    }
                }
                _ => {}
            },
            OStmt::Expr(e) => {
                let _ = self.type_of_expr_oast(&e.expr)?;
            }
            OStmt::If(i) => {
                let _ = self.type_of_expr_oast(&i.test)?;
                self.check_statement_oast(&i.cons)?;
                if let Some(alt) = &i.alt {
                    self.check_statement_oast(alt)?;
                }
            }
            OStmt::While(w) => {
                let _ = self.type_of_expr_oast(&w.test)?;
                self.check_statement_oast(&w.body)?;
            }
            OStmt::Return(r) => {
                if let Some(arg) = &r.arg {
                    let _ = self.type_of_expr_oast(arg)?;
                }
            }
            _ => {}
        }
        Ok(())
    }

    fn type_of_expr_oast(&self, expr: &oast::Expr) -> Result<Type, TypeCheckerError> {
        use oast::Expr as OExpr;
        match expr {
            OExpr::Lit(l) => Ok(match &l.kind {
                oast::LitKind::Number(_) => Type::Number,
                oast::LitKind::String(s) => Type::String,
                oast::LitKind::Bool(b) => Type::Boolean,
                oast::LitKind::Null => Type::Null,
                _ => Type::Unknown,
            }),
            OExpr::Ident(id) => {
                let name = &id.name;
                self.lookup(name)
                    .ok_or_else(|| TypeCheckerError::from(format!("Unknown variable '{}'", name)))
            }
            OExpr::Unary(u) => {
                let a = self.type_of_expr_oast(&u.arg)?;
                match u.op {
                    oast::UnaryOp::Not => Ok(Type::Boolean),
                    oast::UnaryOp::Neg => {
                        if assignable(&Type::Number, &a) {
                            Ok(Type::Number)
                        } else {
                            Err(TypeCheckerError::from("- unary requires number"))
                        }
                    }
                    _ => Ok(Type::Unknown),
                }
            }
            OExpr::Bin(b) => {
                let l = self.type_of_expr_oast(&b.lhs)?;
                let r = self.type_of_expr_oast(&b.rhs)?;
                match b.op {
                    oast::BinaryOp::Add => {
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
                    oast::BinaryOp::Sub | oast::BinaryOp::Mul | oast::BinaryOp::Div => {
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
                    _ => Ok(Type::Unknown),
                }
            }
            OExpr::Call(c) => {
                let callee_type = self.type_of_expr_oast(&c.callee)?;
                let is_console_log = if let OExpr::Member(m) = &*c.callee {
                    if let OExpr::Ident(obj_ident) = &*m.obj {
                        let obj = &obj_ident.name;
                        if let oast::PropName::Ident(prop_ident) = &m.prop {
                            obj == "console" && prop_ident.name == "log"
                        } else {
                            false
                        }
                    } else {
                        false
                    }
                } else {
                    false
                };
                match &callee_type {
                    Type::Function(params, ret) => {
                        if !params.is_empty() && params.len() != c.args.len() {
                            return Err(TypeCheckerError::from(format!(
                                "Expected {} arguments, got {}",
                                params.len(),
                                c.args.len()
                            )));
                        }
                        for (i, (arg, param)) in c.args.iter().zip(params.iter()).enumerate() {
                            let arg_type = self.type_of_expr_oast(arg)?;
                            if !assignable(param, &arg_type) {
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
            OExpr::Member(m) => {
                let obj_type = self.type_of_expr_oast(&m.obj)?;
                match obj_type {
                    Type::Object(ref props) => {
                        let key_opt = if !m.computed {
                            match &m.prop {
                                oast::PropName::Ident(ident) => Some(ident.name.clone()),
                                oast::PropName::Str(s) => Some(s.value.clone()),
                                _ => None,
                            }
                        } else {
                            None
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
                            Err(TypeCheckerError::from(
                                "Unsupported property access in type context",
                            ))
                        }
                    }
                    _ => Err(TypeCheckerError::from("GetProperty on non-object type")),
                }
            }
            OExpr::Array(arr) => {
                if arr.elements.is_empty() {
                    return Err(TypeCheckerError::from(
                        "Empty array literal requires an explicit type annotation",
                    ));
                }
                let first = self.type_of_expr_oast(&arr.elements[0])?;
                for e in &arr.elements[1..] {
                    let t = self.type_of_expr_oast(e)?;
                    if !assignable(&first, &t) && !assignable(&t, &first) {
                        return Err(TypeCheckerError::from("Array elements must have same type"));
                    }
                }
                Ok(Type::Array(Box::new(first)))
            }
            OExpr::This => Ok(Type::Object(HashMap::new())),
            OExpr::Object(obj) => {
                let mut props = HashMap::new();
                for p in &obj.properties {
                    if let oast::Prop::KeyValue(k, v) = p {
                        let t = self.type_of_expr_oast(v)?;
                        match k {
                            oast::PropName::Ident(ident) => {
                                props.insert(ident.name.clone(), t);
                            }
                            oast::PropName::Str(s) => {
                                props.insert(s.value.clone(), t);
                            }
                            _ => {}
                        }
                    }
                }
                Ok(Type::Object(props))
            }
            _ => Ok(Type::Unknown),
        }
    }
}

fn convert_type_ann(_ty: &oast::TsType) -> Result<Type, TypeCheckerError> {
    // For now, be permissive: map many annotations to simple Type variants.
    // A full mapping is out of scope for this migration patch.
    Ok(Type::Any)
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

pub fn type_to_string(t: &Type) -> String {
    match t {
        Type::Number => "number".to_string(),
        Type::String => "string".to_string(),
        Type::Boolean => "boolean".to_string(),
        Type::Null => "null".to_string(),
        Type::Void => "void".to_string(),
        Type::Any => "any".to_string(),
        Type::Unknown => "unknown".to_string(),
        Type::Array(inner) => format!("{}[]", type_to_string(inner)),
        Type::Object(_) => "object".to_string(),
        Type::Function(params, ret) => format!(
            "fn({}) -> {}",
            params
                .iter()
                .map(type_to_string)
                .collect::<Vec<_>>()
                .join(", "),
            type_to_string(ret)
        ),
        Type::TypeReference { name } => name.clone(),
        Type::Union(v) => v.iter().map(type_to_string).collect::<Vec<_>>().join(" | "),
        Type::Builtin(b) => format!("builtin::{:?}", b),
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
        let session = crate::parser::Parser::parse_source(src).expect("parse ok");
        let program = session.program();
        assert!(check_oxc(program).is_ok());
    }
}
