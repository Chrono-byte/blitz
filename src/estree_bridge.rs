//! ESTree JSON -> local_ast converter.
//!
//! This module converts the ESTree JSON produced by Oxc
//! (via `Program::to_pretty_estree_ts_json`) into the owned
//! `crate::local_ast` shapes used by the legacy compiler/typechecker.
//!
//! The converter implements a conservative subset sufficient for the
//! current compiler and typechecker tests (functions, vars, basic
//! expressions, control flow). Unknown nodes return an error to make
//! gaps apparent during iterative migration.

use serde_json::Value;

use crate::local_ast as la;

pub fn estree_json_to_local_program(json: &str) -> Result<la::Program, String> {
    let root: Value =
        serde_json::from_str(json).map_err(|e| format!("Failed to parse ESTree JSON: {}", e))?;

    let body = root
        .get("body")
        .and_then(|b| b.as_array())
        .ok_or_else(|| "Expected program 'body' array".to_string())?;

    let mut stmts = Vec::new();
    for item in body {
        stmts.push(convert_stmt(item)?);
    }
    Ok(la::Program::new(stmts))
}

fn convert_stmt(v: &Value) -> Result<la::Statement, String> {
    let typ = v
        .get("type")
        .and_then(|t| t.as_str())
        .ok_or_else(|| "Missing node 'type'".to_string())?;

    match typ {
        "ImportDeclaration" => {
            let source = v
                .get("source")
                .and_then(|s| s.get("value"))
                .and_then(|sv| sv.as_str())
                .ok_or_else(|| "import missing source".to_string())?
                .to_string();
            let mut specs = Vec::new();
            if let Some(specs_arr) = v.get("specifiers").and_then(|s| s.as_array()) {
                for s in specs_arr {
                    let stype = s.get("type").and_then(|t| t.as_str()).unwrap_or("");
                    // local name is present on all specifier kinds
                    let local = s
                        .get("local")
                        .and_then(|l| l.get("name"))
                        .and_then(|n| n.as_str())
                        .map(ToString::to_string)
                        .ok_or_else(|| "import specifier missing local name".to_string())?;
                    let imported = match stype {
                        "ImportDefaultSpecifier" => "default".to_string(),
                        "ImportNamespaceSpecifier" => "*".to_string(),
                        _ => s
                            .get("imported")
                            .and_then(|i| i.get("name"))
                            .and_then(|n| n.as_str()).map_or_else(|| local.clone(), ToString::to_string),
                    };
                    specs.push((local, imported));
                }
            }
            Ok(la::Statement::ImportDecl(la::ImportDecl {
                specifiers: specs,
                source,
            }))
        }
        "ExportNamedDeclaration" | "ExportAllDeclaration" => {
            // handle simple export forms and re-exports
            let source = v
                .get("source")
                .and_then(|s| s.get("value"))
                .and_then(|sv| sv.as_str())
                .map(ToString::to_string);
            let decl = v
                .get("declaration")
                .and_then(|d| if d.is_null() { None } else { Some(d) });
            let declaration = match decl {
                Some(dv) => Some(Box::new(convert_stmt(dv)?)),
                None => None,
            };

            // collect exported specifiers when present: (exported, local)
            let mut specs: Vec<(String, Option<String>)> = Vec::new();
            if let Some(specs_arr) = v.get("specifiers").and_then(|s| s.as_array()) {
                for sp in specs_arr {
                    if sp.get("type").and_then(|t| t.as_str()) == Some("ExportSpecifier") {
                        let exported = sp
                            .get("exported")
                            .and_then(|e| e.get("name"))
                            .and_then(|n| n.as_str())
                            .map(ToString::to_string)
                            .or_else(|| {
                                // exported can be a literal (string) in some forms
                                sp.get("exported")
                                    .and_then(|e| e.get("value"))
                                    .and_then(|v| v.as_str())
                                    .map(ToString::to_string)
                            });
                        let local = sp
                            .get("local")
                            .and_then(|l| l.get("name"))
                            .and_then(|n| n.as_str())
                            .map(ToString::to_string);
                        if let Some(exp) = exported {
                            specs.push((exp, local));
                        }
                    }
                }
            }

            // preserve a single 'name' for compatibility: prefer first exported name
            let name = specs.first().map(|(e, _)| e.clone());

            Ok(la::Statement::ExportDecl(la::ExportDecl {
                name,
                declaration,
                source,
                specifiers: specs,
            }))
        }
        "ExportDefaultDeclaration" => {
            // export default <declaration|expression>
            if let Some(dv) = v.get("declaration") {
                // If the declaration is a FunctionDeclaration or ClassDeclaration,
                // convert and hoist the contained declaration. If anonymous, wrap
                // into a VarDecl named __default_export.
                if let Some(kind) = dv.get("type").and_then(|t| t.as_str()) {
                    match kind {
                        "FunctionDeclaration" | "ClassDeclaration" => {
                            // Attempt to convert as a statement; if it's an anonymous
                            // declaration, create a named var to hold it.
                            match convert_stmt(dv) {
                                Ok(stmt) => {
                                    // If the contained declaration has a local name, record
                                    // a default specifier mapping (default -> local_name).
                                    let mut specs = Vec::new();
                                    match &stmt {
                                        la::Statement::FunctionDecl(f) => {
                                            specs.push((
                                                "default".to_string(),
                                                Some(f.name.clone()),
                                            ));
                                        }
                                        la::Statement::VarDecl(v) => {
                                            specs.push((
                                                "default".to_string(),
                                                Some(v.name.clone()),
                                            ));
                                        }
                                        la::Statement::ClassDecl(c) => {
                                            specs.push((
                                                "default".to_string(),
                                                Some(c.name.clone()),
                                            ));
                                        }
                                        _ => {}
                                    }
                                    return Ok(la::Statement::ExportDecl(la::ExportDecl {
                                        name: None,
                                        declaration: Some(Box::new(stmt)),
                                        source: None,
                                        specifiers: specs,
                                    }));
                                }
                                Err(_) => {
                                    // fallthrough to wrap as var
                                }
                            }
                        }
                        _ => {}
                    }
                }
                // Fallback: create a var binding for the default export and
                // record a default specifier mapping.
                let default_var = la::Statement::VarDecl(la::VarDecl {
                    name: "__default_export".to_string(),
                    type_ann: None,
                    init: Some(convert_expr(dv)?),
                });
                return Ok(la::Statement::ExportDecl(la::ExportDecl {
                    name: Some("__default_export".to_string()),
                    declaration: Some(Box::new(default_var)),
                    source: None,
                    specifiers: vec![("default".to_string(), Some("__default_export".to_string()))],
                }));
            }
            Err("Unsupported export default form".to_string())
        }
        "ClassDeclaration" => {
            let id = v
                .get("id")
                .and_then(|i| i.get("name"))
                .and_then(|n| n.as_str())
                .ok_or_else(|| "class missing id".to_string())?;
            Ok(la::Statement::ClassDecl(la::ClassDecl {
                name: id.to_string(),
            }))
        }
        "VariableDeclaration" => {
            // If this declaration is a `declare` form, treat it as a top-level Declare
            if v.get("declare").and_then(Value::as_bool) == Some(true) {
                // Best-effort: pull the declared name where possible
                if let Some(id) = v
                    .get("declarations")
                    .and_then(|d| d.as_array())
                    .and_then(|arr| arr.first())
                    .and_then(|dec| dec.get("id"))
                    && let Some(name) = id.get("name").and_then(|n| n.as_str()) {
                        return Ok(la::Statement::Declare(la::Declare {
                            name: name.to_string(),
                            type_ann: None,
                        }));
                    }
                return Err("Unsupported declare form".to_string());
            }

            // handle simple single-declarator case
            let decls = v
                .get("declarations")
                .and_then(|d| d.as_array())
                .ok_or_else(|| "VariableDeclaration missing 'declarations' array".to_string())?;
            // Convert each declarator into a VarDecl statement (emit multiple statements if needed)
            if decls.len() == 1 {
                let dec = &decls[0];
                let id = dec
                    .get("id")
                    .ok_or_else(|| "Declarator missing id".to_string())?;
                let name = id
                    .get("name")
                    .and_then(|n| n.as_str())
                    .ok_or_else(|| "Unsupported declarator id".to_string())?
                    .to_string();
                let init = dec
                    .get("init")
                    .and_then(|i| if i.is_null() { None } else { Some(i) });
                let init_expr = match init {
                    Some(e) => Some(convert_expr(e)?),
                    None => None,
                };
                Ok(la::Statement::VarDecl(la::VarDecl {
                    name,
                    type_ann: None,
                    init: init_expr,
                }))
            } else {
                Err("VariableDeclaration with multiple declarators not supported yet".to_string())
            }
        }
        "FunctionDeclaration" => {
            let id = v
                .get("id")
                .ok_or_else(|| "FunctionDeclaration missing id".to_string())?;
            let name = id
                .get("name")
                .and_then(|n| n.as_str())
                .ok_or_else(|| "Unsupported function id".to_string())?
                .to_string();
            let params = v
                .get("params")
                .and_then(|p| p.as_array())
                .ok_or_else(|| "FunctionDeclaration missing params".to_string())?;
            let mut ps = Vec::new();
            for p in params {
                ps.push(convert_param(p)?);
            }
            let body = v
                .get("body")
                .ok_or_else(|| "FunctionDeclaration missing body".to_string())?;
            let block = convert_block(body)?;
            Ok(la::Statement::FunctionDecl(la::FunctionDecl {
                name,
                params: ps,
                body: block,
                return_type: None,
            }))
        }
        "BlockStatement" => Ok(la::Statement::Block(convert_block(v)?)),
        "IfStatement" => {
            let test = convert_expr(v.get("test").ok_or_else(|| "If missing test".to_string())?)?;
            let cons = convert_stmt(
                v.get("consequent")
                    .ok_or_else(|| "If missing consequent".to_string())?,
            )?;
            let alt = match v.get("alternate") {
                Some(a) if !a.is_null() => Some(Box::new(convert_stmt(a)?)),
                _ => None,
            };
            Ok(la::Statement::If(la::IfStmt {
                test,
                consequent: Box::new(cons),
                alternate: alt,
            }))
        }
        "TryStatement" => {
            let block = convert_block(
                v.get("block")
                    .ok_or_else(|| "try missing block".to_string())?,
            )?;
            let handler = match v.get("handler") {
                Some(h) if !h.is_null() => {
                    let param = h
                        .get("param")
                        .and_then(|p| p.get("name"))
                        .and_then(|n| n.as_str())
                        .map(ToString::to_string);
                    let body = h
                        .get("body")
                        .ok_or_else(|| "catch missing body".to_string())?;
                    if let Some(name) = param {
                        Some((name, convert_block(body)?))
                    } else {
                        None
                    }
                }
                _ => None,
            };
            let finalizer = match v.get("finalizer") {
                Some(f) if !f.is_null() => Some(convert_block(f)?),
                _ => None,
            };
            Ok(la::Statement::Try(la::TryStmt {
                block,
                handler,
                finalizer,
            }))
        }
        "ThrowStatement" => {
            let arg = convert_expr(
                v.get("argument")
                    .ok_or_else(|| "throw missing argument".to_string())?,
            )?;
            Ok(la::Statement::Throw(la::ThrowStmt { argument: arg }))
        }
        "WhileStatement" => {
            let test = convert_expr(
                v.get("test")
                    .ok_or_else(|| "while missing test".to_string())?,
            )?;
            let body = convert_stmt(
                v.get("body")
                    .ok_or_else(|| "while missing body".to_string())?,
            )?;
            Ok(la::Statement::While(la::WhileStmt {
                test,
                body: Box::new(body),
            }))
        }
        "ContinueStatement" => Ok(la::Statement::Continue),
        "ReturnStatement" => {
            let arg = match v.get("argument") {
                Some(a) if !a.is_null() => Some(convert_expr(a)?),
                _ => None,
            };
            Ok(la::Statement::Return(la::ReturnStmt { argument: arg }))
        }
        "ExpressionStatement" => {
            let expr = convert_expr(
                v.get("expression")
                    .ok_or_else(|| "expr stmt missing expression".to_string())?,
            )?;
            Ok(la::Statement::ExpressionStatement(
                la::ExpressionStatement { expression: expr },
            ))
        }
        "EmptyStatement" => Err("EmptyStatement not supported".to_string()),
        "TSDeclareFunction" | "DeclareVariable"
            if v.get("declare").and_then(Value::as_bool) == Some(true) =>
        {
            // Best-effort: pull the declared name where possible
            if let Some(id) = v.get("id").or_else(|| {
                v.get("declarations")
                    .and_then(|d| d.as_array())
                    .and_then(|arr| arr.first())
                    .and_then(|dec| dec.get("id"))
            })
                && let Some(name) = id.get("name").and_then(|n| n.as_str()) {
                    return Ok(la::Statement::Declare(la::Declare {
                        name: name.to_string(),
                        type_ann: None,
                    }));
                }
            Err("Unsupported declare form".to_string())
        }
        other => Err(format!("Unsupported statement type: {}", other)),
    }
}

fn convert_block(v: &Value) -> Result<la::Block, String> {
    let body = v
        .get("body")
        .and_then(|b| b.as_array())
        .ok_or_else(|| "Block missing body".to_string())?;
    let mut stmts = Vec::new();
    for s in body {
        stmts.push(convert_stmt(s)?);
    }
    Ok(la::Block { body: stmts })
}

fn convert_param(v: &Value) -> Result<la::Param, String> {
    // params are usually Identifier or RestElement
    if v.get("type").and_then(|t| t.as_str()) == Some("Identifier") {
        let name = v
            .get("name")
            .and_then(|n| n.as_str())
            .ok_or_else(|| "param missing name".to_string())?;
        Ok(la::Param {
            name: name.to_string(),
            type_ann: None,
            rest: false,
        })
    } else if v.get("type").and_then(|t| t.as_str()) == Some("RestElement") {
        let arg = v
            .get("argument")
            .ok_or_else(|| "rest missing argument".to_string())?;
        let name = arg
            .get("name")
            .and_then(|n| n.as_str())
            .ok_or_else(|| "rest arg missing name".to_string())?;
        Ok(la::Param {
            name: name.to_string(),
            type_ann: None,
            rest: true,
        })
    } else {
        Err(format!("Unsupported param node: {:?}", v.get("type")))
    }
}

fn convert_expr(v: &Value) -> Result<la::Expr, String> {
    let typ = v
        .get("type")
        .and_then(|t| t.as_str())
        .ok_or_else(|| "expr missing type".to_string())?;
    match typ {
        "Literal" => {
            // value may be number, string, bool, or null
            if let Some(val) = v.get("value") {
                if val.is_null() {
                    return Ok(la::Expr::Literal(la::Literal {
                        value: la::LiteralValue::Null,
                    }));
                }

                if let Some(n) = val.as_f64() {
                    return Ok(la::Expr::Literal(la::Literal {
                        value: la::LiteralValue::Number(n),
                    }));
                }

                if let Some(s) = val.as_str() {
                    return Ok(la::Expr::Literal(la::Literal {
                        value: la::LiteralValue::String(s.to_string()),
                    }));
                }

                if let Some(b) = val.as_bool() {
                    return Ok(la::Expr::Literal(la::Literal {
                        value: la::LiteralValue::Bool(b),
                    }));
                }
            }
            Err("Unsupported literal value".to_string())
        }
        "Identifier" => {
            let name = v
                .get("name")
                .and_then(|n| n.as_str())
                .ok_or_else(|| "identifier missing name".to_string())?;
            Ok(la::Expr::Identifier(la::Identifier {
                name: name.to_string(),
            }))
        }
        "ThisExpression" => Ok(la::Expr::ThisExpression),
        "BinaryExpression" => {
            let left = convert_expr(
                v.get("left")
                    .ok_or_else(|| "binary missing left".to_string())?,
            )?;
            let right = convert_expr(
                v.get("right")
                    .ok_or_else(|| "binary missing right".to_string())?,
            )?;
            let op = match v
                .get("operator")
                .and_then(|o| o.as_str())
                .ok_or_else(|| "binary missing operator".to_string())?
            {
                "+" => la::BinaryOperator::Plus,
                "-" => la::BinaryOperator::Minus,
                "*" => la::BinaryOperator::Star,
                "/" => la::BinaryOperator::Slash,
                "%" => la::BinaryOperator::Percent,
                "==" => la::BinaryOperator::EqEq,
                "!=" => la::BinaryOperator::BangEq,
                "===" => la::BinaryOperator::EqEqEq,
                "!==" => la::BinaryOperator::BangEqEq,
                "<" => la::BinaryOperator::Lt,
                "<=" => la::BinaryOperator::Le,
                ">" => la::BinaryOperator::Gt,
                ">=" => la::BinaryOperator::Ge,
                "&&" => la::BinaryOperator::AndAnd,
                "||" => la::BinaryOperator::OrOr,
                other => return Err(format!("Unsupported binary operator: {}", other)),
            };
            Ok(la::Expr::BinaryExpr(la::BinaryExpr {
                left: Box::new(left),
                right: Box::new(right),
                operator: op,
            }))
        }
        "UnaryExpression" => {
            let arg = convert_expr(
                v.get("argument")
                    .ok_or_else(|| "unary missing argument".to_string())?,
            )?;
            let op = match v
                .get("operator")
                .and_then(|o| o.as_str())
                .ok_or_else(|| "unary missing operator".to_string())?
            {
                "!" => la::UnaryOperator::Bang,
                "-" => la::UnaryOperator::Minus,
                "typeof" => la::UnaryOperator::Typeof,
                other => return Err(format!("Unsupported unary operator: {}", other)),
            };
            Ok(la::Expr::UnaryExpr(la::UnaryExpr {
                operator: op,
                argument: Box::new(arg),
            }))
        }
        "CallExpression" => {
            let callee = convert_expr(
                v.get("callee")
                    .ok_or_else(|| "call missing callee".to_string())?,
            )?;
            let args = v
                .get("arguments")
                .and_then(|a| a.as_array())
                .ok_or_else(|| "call missing arguments".to_string())?;
            let mut asv = Vec::new();
            for a in args {
                // spread arguments are not supported here
                asv.push(convert_expr(a)?);
            }
            Ok(la::Expr::CallExpr(la::CallExpr {
                callee: Box::new(callee),
                args: asv,
            }))
        }
        "MemberExpression" => {
            let object = convert_expr(
                v.get("object")
                    .ok_or_else(|| "member missing object".to_string())?,
            )?;
            let property = convert_expr(
                v.get("property")
                    .ok_or_else(|| "member missing property".to_string())?,
            )?;
            let computed = v.get("computed").and_then(Value::as_bool).unwrap_or(false);
            Ok(la::Expr::MemberExpr(la::MemberExpr {
                object: Box::new(object),
                property: Box::new(property),
                computed,
            }))
        }
        "AssignmentExpression" => {
            let left = v
                .get("left")
                .ok_or_else(|| "assign missing left".to_string())?;
            let target = match left.get("type").and_then(|t| t.as_str()) {
                Some("Identifier") => {
                    let name = left
                        .get("name")
                        .and_then(|n| n.as_str())
                        .ok_or_else(|| "assign id missing name".to_string())?;
                    la::AssignmentTarget::Identifier(la::Identifier {
                        name: name.to_string(),
                    })
                }
                Some("MemberExpression") => {
                    let me = convert_expr(left)?;
                    if let la::Expr::MemberExpr(m) = me {
                        la::AssignmentTarget::MemberExpr(m)
                    } else {
                        return Err("Left side MemberExpression conversion failed".to_string());
                    }
                }
                _ => return Err("Unsupported assignment left-hand side".to_string()),
            };
            let value = convert_expr(
                v.get("right")
                    .ok_or_else(|| "assign missing right".to_string())?,
            )?;
            Ok(la::Expr::Assignment(la::AssignmentExpr {
                target,
                value: Box::new(value),
            }))
        }
        "ArrayExpression" => {
            let els = v
                .get("elements")
                .and_then(|a| a.as_array())
                .ok_or_else(|| "array missing elements".to_string())?;
            let mut ev = Vec::new();
            for e in els {
                ev.push(convert_expr(e)?);
            }
            Ok(la::Expr::ArrayExpr(la::ArrayExpr { elements: ev }))
        }
        "ObjectExpression" => {
            let props = v
                .get("properties")
                .and_then(|p| p.as_array())
                .ok_or_else(|| "object missing properties".to_string())?;
            let mut pv = Vec::new();
            for p in props {
                let key = if let Some(k) = p.get("key") {
                    if k.get("type").and_then(|t| t.as_str()) == Some("Identifier") {
                        k.get("name")
                            .and_then(|n| n.as_str())
                            .ok_or_else(|| "prop id missing name".to_string())?
                            .to_string()
                    } else if k.get("type").and_then(|t| t.as_str()) == Some("Literal") {
                        k.get("value")
                            .and_then(|v| v.as_str())
                            .ok_or_else(|| "prop literal key missing value".to_string())?
                            .to_string()
                    } else {
                        return Err("Unsupported object property key".to_string());
                    }
                } else {
                    return Err("Property missing key".to_string());
                };
                let value = p
                    .get("value")
                    .ok_or_else(|| "property missing value".to_string())?;
                let expr = convert_expr(value)?;
                pv.push(la::Property { key, value: expr });
            }
            Ok(la::Expr::ObjectExpr(la::ObjectExpr { properties: pv }))
        }
        "FunctionExpression" => {
            let params = v
                .get("params")
                .and_then(|p| p.as_array())
                .ok_or_else(|| "fn expr missing params".to_string())?;
            let mut ps = Vec::new();
            for p in params {
                ps.push(convert_param(p)?);
            }
            let body = v
                .get("body")
                .ok_or_else(|| "fn expr missing body".to_string())?;
            let block = convert_block(body)?;
            Ok(la::Expr::FunctionExpr(la::FunctionExpr {
                params: ps,
                body: block,
                return_type: None,
            }))
        }
        other => Err(format!("Unsupported expression type: {}", other)),
    }
}
