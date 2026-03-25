pub mod prelude;
pub mod syntax;

use prelude::*;

pub fn parse(input: &str) -> () {
    println!("You called parse with the input: {}", input);

    println!();

    println!("Parse Identifier");
    println!("Result: {:#?}", Ident::parse(input));

    println!();

    println!("Parse Type");
    println!("Result: {:#?}", Type::parse(input));

    println!();

    println!("Parse Statement");
    println!("Result: {:#?}", Statement::parse(input));

    println!();

    println!("Parse Expression");
    println!("Result: {:#?}", Expression::parse(input));

    println!();

    println!("Parse Root Statement");
    println!("Result: {:#?}", RootStatement::parse(input));
}
