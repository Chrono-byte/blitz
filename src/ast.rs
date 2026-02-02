//! AST Definition for blitz
//! Compatible with TypeScript layer for serialization

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

// --- Types (first-class; not erased) ---

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
/// Type enum (compile-time only, erased at runtime)
pub enum Type {
    /// Number type
    Number,
    /// String type
    String,
    /// Boolean type
    Boolean,
    /// Null type
    Null,
    /// Void type
    Void,
    /// Array type
    Array(Box<Type>),
    /// Object type: map of property name -> type
    Object(HashMap<String, Type>),
    /// (param types) => return type
    Function(Vec<Type>, Box<Type>),
}

#[derive(Clone, Debug, Serialize, Deserialize)]
/// Program struct
pub struct Program {
    /// Program kind: "Program" for scripts, "Module" for ES modules
    pub kind: ProgramKind,
    /// Type strictness level
    pub type_strictness: TypeStrictness,
    /// List of top-level statements
    pub body: Vec<Statement>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
/// Program kind
pub enum ProgramKind {
    /// Script program
    #[serde(rename = "Program")]
    Program,
    /// ES module program
    #[serde(rename = "Module")]
    Module,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
/// Type strictness level
pub enum TypeStrictness {
    /// Strict TypeScript checking
    #[serde(rename = "strict")]
    Strict,
    /// Soft TypeScript checking
    #[serde(rename = "soft")]
    Soft,
    /// Dynamic JavaScript (no types)
    #[serde(rename = "dynamic")]
    Dynamic,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
/// Statement enum
#[serde(tag = "kind")]
pub enum Statement {
    /// Variable declaration
    #[serde(rename = "VarDecl")]
    VarDecl(VarDecl),
    /// Ambient/declare variable (no runtime emission)
    #[serde(rename = "Declare")]
    Declare(VarDecl),
    /// Block statement
    #[serde(rename = "Block")]
    Block(Block),
    /// If statement
    #[serde(rename = "If")]
    If(If),
    /// While statement
    #[serde(rename = "While")]
    While(While),
    /// Function declaration
    #[serde(rename = "FunctionDecl")]
    FunctionDecl(FunctionDecl),
    /// Return statement
    #[serde(rename = "Return")]
    Return(Return),
    /// Throw statement
    #[serde(rename = "Throw")]
    Throw(Throw),
    /// Try/Catch/Finally statement
    #[serde(rename = "Try")]
    Try(TryStatement),
    /// Expression statement
    #[serde(rename = "ExpressionStatement")]
    ExpressionStatement(ExpressionStatement),
}

#[derive(Clone, Debug, Serialize, Deserialize)]
/// Variable declaration struct
pub struct VarDecl {
    /// Variable name
    pub name: String,
    /// Variable type annotation (compile-time only)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub type_ann: Option<Type>,
    /// Variable initializer expression
    #[serde(skip_serializing_if = "Option::is_none")]
    pub init: Option<Expr>,
    /// Whether variable is mutable (let vs const)
    pub mutable: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
/// Block struct
pub struct Block {
    /// List of statements within the block
    pub body: Vec<Statement>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
/// If statement struct
pub struct If {
    /// If statement condition
    pub test: Expr,
    /// If statement consequent statement
    pub consequent: Box<Statement>,
    /// If statement alternate statement
    #[serde(skip_serializing_if = "Option::is_none")]
    pub alternate: Option<Box<Statement>>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
/// While statement struct
pub struct While {
    /// While statement condition
    pub test: Expr,
    /// While statement body statement
    pub body: Box<Statement>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
/// Parameter struct
pub struct Param {
    /// Parameter name
    pub name: String,
    /// Parameter type annotation (compile-time only)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub type_ann: Option<Type>,
    /// Whether this parameter is a rest parameter (`...args`). Not emitted at runtime.
    #[serde(default)]
    pub rest: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
/// Function declaration struct
pub struct FunctionDecl {
    /// Function declaration name
    pub name: String,
    /// Function declaration parameters
    pub params: Vec<Param>,
    /// Function declaration return type annotation (compile-time only)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub return_type: Option<Type>,
    /// Function declaration body
    pub body: Block,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
/// Return statement
pub struct Return {
    /// Return statement argument
    #[serde(skip_serializing_if = "Option::is_none")]
    pub argument: Option<Expr>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
/// Throw statement
pub struct Throw {
    /// Throw argument expression
    pub argument: Expr,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
/// Try/Catch/Finally structure (simplified)
pub struct TryStatement {
    /// The try block
    pub block: Block,
    /// Optional catch parameter name and catch block
    #[serde(skip_serializing_if = "Option::is_none")]
    pub handler: Option<(String, Block)>,
    /// Optional finally block
    #[serde(skip_serializing_if = "Option::is_none")]
    pub finalizer: Option<Block>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
/// Expression statement
pub struct ExpressionStatement {
    /// The expression
    pub expression: Expr,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
/// Expression enum
#[serde(tag = "kind")]
pub enum Expr {
    /// Literal expression
    #[serde(rename = "Literal")]
    Literal(Literal),
    /// Identifier expression
    #[serde(rename = "Identifier")]
    Identifier(Identifier),
    /// This expression
    #[serde(rename = "ThisExpression")]
    ThisExpression,
    /// Binary expression
    #[serde(rename = "BinaryExpr")]
    BinaryExpr(BinaryExpr),
    /// Unary expression
    #[serde(rename = "UnaryExpr")]
    UnaryExpr(UnaryExpr),
    /// Call expression
    #[serde(rename = "CallExpr")]
    CallExpr(CallExpr),
    /// Member expression
    #[serde(rename = "MemberExpr")]
    MemberExpr(MemberExpr),
    /// Assignment expression
    #[serde(rename = "Assignment")]
    Assignment(AssignmentExpr),
    /// Array expression
    #[serde(rename = "ArrayExpr")]
    ArrayExpr(ArrayExpr),
    /// Object expression
    #[serde(rename = "ObjectExpr")]
    ObjectExpr(ObjectExpr),
    /// Function expression
    #[serde(rename = "FunctionExpr")]
    FunctionExpr(FunctionExpr),
}

#[derive(Clone, Debug, Serialize, Deserialize)]
/// Literal struct
pub struct Literal {
    /// Literal value
    pub value: LiteralValue,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
/// Literal value enum
#[serde(untagged)]
pub enum LiteralValue {
    /// Number literal
    Number(f64),
    /// String literal
    String(String),
    /// Boolean literal
    Bool(bool),
    /// Null literal
    Null,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
/// Identifier struct
pub struct Identifier {
    /// Identifier name
    pub name: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
/// Binary expression struct
pub struct BinaryExpr {
    /// Binary operator
    pub operator: BinaryOperator,
    /// Left operand
    pub left: Box<Expr>,
    /// Right operand
    pub right: Box<Expr>,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
/// Binary operator enum
pub enum BinaryOperator {
    /// Addition (+)
    #[serde(rename = "+")]
    Plus,
    /// Subtraction (-)
    #[serde(rename = "-")]
    Minus,
    /// Multiplication (*)
    #[serde(rename = "*")]
    Star,
    /// Division (/)
    #[serde(rename = "/")]
    Slash,
    /// Modulo (%)
    #[serde(rename = "%")]
    Percent,
    /// Loose equality (==)
    #[serde(rename = "==")]
    EqEq,
    /// Loose inequality (!=)
    #[serde(rename = "!=")]
    BangEq,
    /// Strict equality (===)
    #[serde(rename = "===")]
    EqEqEq,
    /// Strict inequality (!==)
    #[serde(rename = "!==")]
    BangEqEq,
    /// Less than (<)
    #[serde(rename = "<")]
    Lt,
    /// Less than or equal (<=)
    #[serde(rename = "<=")]
    Le,
    /// Greater than (>)
    #[serde(rename = ">")]
    Gt,
    /// Greater than or equal (>=)
    #[serde(rename = ">=")]
    Ge,
    /// Logical AND (&&)
    #[serde(rename = "&&")]
    AndAnd,
    /// Logical OR (||)
    #[serde(rename = "||")]
    OrOr,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
/// Unary expression struct
pub struct UnaryExpr {
    /// Unary operator
    pub operator: UnaryOperator,
    /// Operand
    pub argument: Box<Expr>,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
/// Unary operator enum
pub enum UnaryOperator {
    /// Logical NOT (!)
    #[serde(rename = "!")]
    Bang,
    /// Unary negation (-)
    #[serde(rename = "-")]
    Minus,
    /// typeof operator
    #[serde(rename = "typeof")]
    Typeof,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
/// Call expression struct
pub struct CallExpr {
    /// Function/expression to call
    pub callee: Box<Expr>,
    /// Arguments to pass to the function
    pub args: Vec<Expr>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
/// Member expression struct
pub struct MemberExpr {
    /// Object being accessed
    pub object: Box<Expr>,
    /// Property being accessed
    pub property: Box<Expr>,
    /// Whether property access is computed (obj[prop] vs obj.prop)
    pub computed: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
/// Assignment expression struct
pub struct AssignmentExpr {
    /// Assignment target
    pub target: AssignmentTarget,
    /// Value to assign
    pub value: Box<Expr>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
/// Assignment target enum
#[serde(untagged)]
pub enum AssignmentTarget {
    /// Identifier assignment target
    Identifier(Identifier),
    /// Member expression assignment target
    MemberExpr(MemberExpr),
}

#[derive(Clone, Debug, Serialize, Deserialize)]
/// Array expression struct
pub struct ArrayExpr {
    /// Array elements
    pub elements: Vec<Expr>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
/// Object expression struct
pub struct ObjectExpr {
    /// Object properties (key-value pairs)
    pub properties: Vec<ObjectProperty>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
/// Object property
pub struct ObjectProperty {
    /// Property key
    pub key: String,
    /// Property value
    pub value: Expr,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
/// Function expression struct
pub struct FunctionExpr {
    /// Function parameters
    pub params: Vec<Param>,
    /// Function return type annotation (compile-time only)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub return_type: Option<Type>,
    /// Function body
    pub body: Block,
}
