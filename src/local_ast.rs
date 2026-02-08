#![allow(missing_docs)]
//! Minimal owned AST used as a compatibility bridge.
//!
//! This module defines a small, owned AST shape that mirrors the legacy
//! internal AST used by the compiler and typechecker. It's intentionally
//! conservative and only contains the node kinds needed by the existing
//! compiler/typecheck code. We construct this AST by converting from Oxc's
//! ESTree JSON representation.

use crate::typecheck::Type;

#[derive(Debug, Clone)]
pub struct Program {
    pub body: Vec<Statement>,
}

#[derive(Debug, Clone)]
pub enum Statement {
    For(ForStmt),
    Declare(Declare),
    VarDecl(VarDecl),
    Block(Block),
    If(IfStmt),
    Try(TryStmt),
    Throw(ThrowStmt),
    While(WhileStmt),
    Continue,
    Break,
    Switch(SwitchStmt),
    FunctionDecl(FunctionDecl),
    ImportDecl(ImportDecl),
    ExportDecl(ExportDecl),
    ClassDecl(ClassDecl),
    Return(ReturnStmt),
    ExpressionStatement(ExpressionStatement),
}

#[derive(Debug, Clone)]
pub struct Declare {
    pub name: String,
    pub type_ann: Option<Type>,
}

#[derive(Debug, Clone)]
pub struct VarDeclarator {
    pub name: String,
    pub type_ann: Option<Type>,
    pub init: Option<Expr>,
}

#[derive(Debug, Clone)]
pub struct VarDecl {
    pub decls: Vec<VarDeclarator>,
}

#[derive(Debug, Clone)]
pub struct Block {
    pub body: Vec<Statement>,
}

#[derive(Debug, Clone)]
pub struct IfStmt {
    pub test: Expr,
    pub consequent: Box<Statement>,
    pub alternate: Option<Box<Statement>>,
}

#[derive(Debug, Clone)]
pub struct TryStmt {
    pub block: Block,
    pub handler: Option<(String, Block)>,
    pub finalizer: Option<Block>,
}

#[derive(Debug, Clone)]
pub struct ThrowStmt {
    pub argument: Expr,
}

#[derive(Debug, Clone)]
pub struct WhileStmt {
    pub test: Expr,
    pub body: Box<Statement>,
}

#[derive(Debug, Clone)]
pub struct ForStmt {
    pub init: Option<Box<Statement>>,
    pub test: Option<Expr>,
    pub update: Option<Expr>,
    pub body: Box<Statement>,
}

#[derive(Debug, Clone)]
pub struct SwitchCase {
    pub test: Option<Expr>,
    pub consequent: Vec<Statement>,
}

#[derive(Debug, Clone)]
pub struct SwitchStmt {
    pub discriminant: Expr,
    pub cases: Vec<SwitchCase>,
}

#[derive(Debug, Clone)]
pub struct FunctionDecl {
    pub name: String,
    pub params: Vec<Param>,
    pub body: Block,
    pub return_type: Option<Type>,
}

#[derive(Debug, Clone)]
pub struct ReturnStmt {
    pub argument: Option<Expr>,
}

#[derive(Debug, Clone)]
pub struct ExpressionStatement {
    pub expression: Expr,
}

#[derive(Debug, Clone)]
pub enum AssignmentTarget {
    Identifier(Identifier),
    MemberExpr(MemberExpr),
}

#[derive(Debug, Clone)]
pub enum Expr {
    UpdateExpr(UpdateExpr),
    Literal(Literal),
    Identifier(Identifier),
    BinaryExpr(BinaryExpr),
    UnaryExpr(UnaryExpr),
    CallExpr(CallExpr),
    MemberExpr(MemberExpr),
    Assignment(AssignmentExpr),
    ArrayExpr(ArrayExpr),
    ObjectExpr(ObjectExpr),
    TemplateExpr(TemplateExpr),
    FunctionExpr(FunctionExpr),
    ThisExpression,
}

#[derive(Debug, Clone)]
pub enum TemplatePart {
    Literal(String),
    Expr(Expr),
}

#[derive(Debug, Clone)]
pub struct TemplateExpr {
    pub parts: Vec<TemplatePart>,
}

#[derive(Debug, Clone)]
pub struct Identifier {
    pub name: String,
}

#[derive(Debug, Clone)]
pub struct Literal {
    pub value: LiteralValue,
}

#[derive(Debug, Clone)]
pub enum LiteralValue {
    Number(f64),
    String(String),
    Bool(bool),
    Null,
}

#[derive(Debug, Clone)]
pub struct BinaryExpr {
    pub left: Box<Expr>,
    pub right: Box<Expr>,
    pub operator: BinaryOperator,
}

#[derive(Debug, Clone)]
pub enum BinaryOperator {
    Plus,
    Minus,
    Star,
    Slash,
    Percent,
    EqEq,
    BangEq,
    EqEqEq,
    BangEqEq,
    Lt,
    Le,
    Gt,
    Ge,
    AndAnd,
    OrOr,
}

#[derive(Debug, Clone)]
pub struct UnaryExpr {
    pub operator: UnaryOperator,
    pub argument: Box<Expr>,
}

#[derive(Debug, Clone)]
pub enum UnaryOperator {
    Bang,
    Minus,
    Typeof,
}

#[derive(Debug, Clone)]
pub struct CallExpr {
    pub callee: Box<Expr>,
    pub args: Vec<Expr>,
}

#[derive(Debug, Clone)]
pub struct MemberExpr {
    pub object: Box<Expr>,
    pub property: Box<Expr>,
    pub computed: bool,
}

#[derive(Debug, Clone)]
pub struct AssignmentExpr {
    pub target: AssignmentTarget,
    pub value: Box<Expr>,
}

#[derive(Debug, Clone)]
pub enum UpdateOperator {
    Inc,
    Dec,
}

#[derive(Debug, Clone)]
pub struct UpdateExpr {
    pub operator: UpdateOperator,
    pub argument: Box<Expr>,
    pub prefix: bool,
}

#[derive(Debug, Clone)]
pub struct ArrayExpr {
    pub elements: Vec<Expr>,
}

#[derive(Debug, Clone)]
pub struct ObjectExpr {
    pub properties: Vec<Property>,
}

#[derive(Debug, Clone)]
pub struct Property {
    pub key: String,
    pub value: Expr,
}

#[derive(Debug, Clone)]
pub struct ImportDecl {
    pub specifiers: Vec<(String, String)>, // (local, imported)
    pub source: String,
}

#[derive(Debug, Clone)]
pub struct ExportDecl {
    pub name: Option<String>,
    pub declaration: Option<Box<Statement>>,
    pub source: Option<String>,
    pub specifiers: Vec<(String, Option<String>)>, // (exported, local?)
}

#[derive(Debug, Clone)]
pub struct ClassDecl {
    pub name: String,
}

#[derive(Debug, Clone)]
pub struct Param {
    pub name: String,
    pub type_ann: Option<Type>,
    pub rest: bool,
}

#[derive(Debug, Clone)]
pub struct FunctionExpr {
    pub params: Vec<Param>,
    pub body: Block,
    pub return_type: Option<Type>,
}

/// Example: construct a minimal `Program` with a numeric literal expression.
///
/// ```
/// let prog = tsblitz::local_ast::Program::new(vec![tsblitz::local_ast::Statement::ExpressionStatement(
///     tsblitz::local_ast::ExpressionStatement { expression: tsblitz::local_ast::Expr::Literal(
///         tsblitz::local_ast::Literal { value: tsblitz::local_ast::LiteralValue::Number(1.0) }
///     ) }
/// )]);
/// assert_eq!(prog.body.len(), 1);
/// ```
impl Program {
    pub fn new(body: Vec<Statement>) -> Self {
        Self { body }
    }
}
