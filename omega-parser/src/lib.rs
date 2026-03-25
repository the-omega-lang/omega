use crate::syntax::SyntaxParser;

pub mod syntax;

pub fn parse(input: &str) -> () {
    println!("You called parse with the input: {}", input);
    println!("Parse Identifier");
    println!("Result: {:?}", syntax::identifier::Ident::parse(input));

    println!();

    println!("Parse Type");
    println!("Result: {:?}", syntax::r#type::Type::parse(input));

    println!("Parse Statement");
    println!("Result: {:?}", syntax::statement::Statement::parse(input));
}
