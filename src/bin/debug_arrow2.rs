use oxc_allocator::Allocator;
use oxc_ast::ast as oast;
use oxc_parser::Parser;

fn main() {
    let allocator = Allocator::default();
    let code = "let f = () => 42;";

    let parser = Parser::new(&allocator, code, Default::default());
    let result = parser.parse();

    for stmt in &result.program.body {
        if let oast::Statement::VariableDeclaration(vd) = stmt {
            for decl in &vd.declarations {
                if let Some(init) = &decl.init {
                    if let oast::Expression::ArrowFunctionExpression(arrow) = init {
                        println!("Arrow function expression flag: {}", arrow.expression);
                        println!("Statement count: {}", arrow.body.statements.len());
                        if let Some(stmt) = arrow.body.statements.first() {
                            println!(
                                "First statement type: {:?}",
                                std::any::type_name_of_val(stmt)
                            );
                        }
                    }
                }
            }
        }
    }
}
