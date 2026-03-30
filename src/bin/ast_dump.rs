//! Temporary binary used to dump AST for debugging.
use blitz::parser::Parser;
use clap::Parser as ClapParser;
use std::fs;

#[derive(ClapParser)]
struct Args {
    path: String,
}

struct Path {
    file: String,
    dir: String,
}

impl Path {
    fn new(path: &str) -> Self {
        let file = std::path::Path::new(path)
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("");
        let dir = std::path::Path::new(path)
            .parent()
            .and_then(|p| p.to_str())
            .unwrap_or("");
        Self {
            file: file.to_string(),
            dir: dir.to_string(),
        }
    }
}

fn main() {
    let args = Args::parse();
    let path = &args.path;
    let src = fs::read_to_string(path).expect("read test");
    let session = Parser::parse_source(&src).expect("parse ok");
    let program = session.program();
    println!("{:#?}", program);
    let p = Path::new(path);
    println!("file: {}", p.file);
    println!("dir: {}", p.dir);
}
