/**
 * Abstract Syntax Tree nodes for the TypeScript VM.
 * Compatible with Rust AST for serialization.
 * Represents a subset of JavaScript/TypeScript (types are erased at runtime).
 */

export type Node =
    | Program
    | VarDecl
    | Block
    | If
    | While
    | FunctionDecl
    | Return
    | ExpressionStatement
    | BinaryExpr
    | UnaryExpr
    | CallExpr
    | MemberExpr
    | Identifier
    | Literal
    | ArrayExpr
    | ObjectExpr
    | Assignment
    | FunctionExpr;

export type ProgramKind = "Program" | "Module";

export type TypeStrictness = "strict" | "soft" | "dynamic";

export interface Program {
    kind: ProgramKind;
    type_strictness: TypeStrictness;
    body: Statement[];
}

export type Statement =
    | VarDecl
    | Block
    | If
    | While
    | FunctionDecl
    | Return
    | ExpressionStatement;

export interface ExpressionStatement {
    kind: "ExpressionStatement";
    expression: Expression;
}

export interface Param {
    name: string;
    // No type annotation at runtime - types are erased
}

export interface VarDecl {
    kind: "VarDecl";
    name: string;
    init: Expression | null;
    mutable: boolean; // let vs const
}

export interface Block {
    kind: "Block";
    body: Statement[];
}

export interface If {
    kind: "If";
    test: Expression;
    consequent: Statement;
    alternate: Statement | null;
}

export interface While {
    kind: "While";
    test: Expression;
    body: Statement;
}

export interface FunctionDecl {
    kind: "FunctionDecl";
    name: string;
    params: Param[];
    body: Block;
}

export interface Return {
    kind: "Return";
    argument: Expression | null;
}

export type Expression =
    | BinaryExpr
    | UnaryExpr
    | CallExpr
    | MemberExpr
    | Identifier
    | Literal
    | ArrayExpr
    | ObjectExpr
    | Assignment
    | FunctionExpr;

export interface FunctionExpr {
    kind: "FunctionExpr";
    params: Param[];
    body: Block;
}

export type BinaryOperator =
    | "+" | "-" | "*" | "/" | "%"
    | "==" | "!=" | "===" | "!=="
    | "<" | "<=" | ">" | ">="
    | "&&" | "||";

export interface BinaryExpr {
    kind: "BinaryExpr";
    operator: BinaryOperator;
    left: Expression;
    right: Expression;
}

export type BinaryOperator =
    | "+" | "-" | "*" | "/" | "%"
    | "==" | "!=" | "===" | "!=="
    | "<" | "<=" | ">" | ">="
    | "&&" | "||";

export type UnaryOperator = "!" | "-" | "typeof";

export interface UnaryExpr {
    kind: "UnaryExpr";
    operator: UnaryOperator;
    argument: Expression;
}

export interface CallExpr {
    kind: "CallExpr";
    callee: Expression;
    args: Expression[];
}

export interface MemberExpr {
    kind: "MemberExpr";
    object: Expression;
    property: Expression;
    computed: boolean; // a[b] vs a.b
}

export interface Identifier {
    kind: "Identifier";
    name: string;
}

export interface Literal {
    kind: "Literal";
    value: string | number | boolean | null;
}

export interface ArrayExpr {
    kind: "ArrayExpr";
    elements: Expression[];
}

export interface ObjectProperty {
    key: string;
    value: Expression;
}

export interface ObjectExpr {
    kind: "ObjectExpr";
    properties: ObjectProperty[];
}

export type AssignmentTarget = Identifier | MemberExpr;

export interface Assignment {
    kind: "Assignment";
    target: AssignmentTarget;
    value: Expression;
}
