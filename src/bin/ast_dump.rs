//! Temporary binary used to dump AST for debugging.
use blitz::parser::Parser;
use std::fs;

fn main() {
    let path = "test262/test/language/expressions/class/elements/after-same-line-static-async-method-private-method-usage.js";
    let src = fs::read_to_string(path).expect("read test");
    let session = Parser::parse_source(&src).expect("parse ok");
    let program = session.program();
    println!("{:#?}", program);
}
