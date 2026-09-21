use sol_tokenizer::model::TokenKind;
use sol_utils::{ids::IdAlloc, span::ModuleId};
extern crate sol_tokenizer;

fn main() {
    let mut tokens = match sol_tokenizer::to_token_stream("hello", ModuleId::begin()) {
        Ok(val) => val,
        Err(err) => {
            eprintln!("{err:?}");
            return;
        }
    };

    loop {
        let (token, fault) = tokens.consume_advance();
        if let Some(err) = fault {
            eprintln!("{err:?}");
        }

        println!("{}", token.kind.display());
        if token.kind == TokenKind::EndFile {
            break;
        }
    }
}
