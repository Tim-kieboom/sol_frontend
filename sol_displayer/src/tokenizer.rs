use anyhow::Result;
use sol_tokenizer::{TokenStream, model::TokenKind};
use sol_utils::{
    collections::module_store::ModuleStore,
    literal::{Number, StringLiteral, TokenLiteral},
};

use crate::{PrintConfigs, fault_to_anyhow_error, push_fmt, writer::Writer};

pub fn display_tokens<'a>(
    tokens: TokenStream<'a>,
    modules: &ModuleStore,
    writer: &mut impl Writer,
    print_configs: &PrintConfigs,
) -> Result<()> {
    for token in tokens {
        let token = token.map_err(|f| fault_to_anyhow_error(&f, modules, print_configs))?;
        let span_str = format!("{:?}", token.span);
        let tab = " ".repeat(30 - span_str.len());

        writer.push_fmt(format_args!("Span({span_str}){tab}>> ",))?;
        display_tokenkind(&token.kind, writer)?;
        writer.push_char('\n')?;
    }

    Ok(writer.writer_flush()?)
}

fn display_tokenkind(token: &TokenKind, writer: &mut impl Writer) -> Result<()> {
    match token {
        TokenKind::Literal(literal) => match literal {
            TokenLiteral::String(str) => match str {
                StringLiteral::Str(str) => push_fmt!(writer, "str({str:?})")?,
                StringLiteral::Cstr(str) => push_fmt!(writer, "cstr({str:?})")?,
            },
            TokenLiteral::Number(num) => match num {
                Number::Int(n) => push_fmt!(writer, "{n}")?,
                Number::Uint(n) => push_fmt!(writer, "{n}")?,
                Number::Float(n) => push_fmt!(writer, "{n}")?,
            },
            TokenLiteral::Char(ch) => push_fmt!(writer, "{ch:?}")?,
        },
        TokenKind::Keyword(key_word) => push_fmt!(writer, "{key_word}")?,
        TokenKind::Symbol(symbol) => push_fmt!(writer, "{symbol}")?,
        TokenKind::Types(types) => push_fmt!(writer, "{types}")?,
        TokenKind::Ident(ident) => push_fmt!(writer, "{ident:?}")?,
        TokenKind::EndLine => writer.push_str("'\\n'")?,
        TokenKind::EndFile => writer.push_str("<end-file>")?,
        TokenKind::StringFormat(tag) => {
            push_fmt!(writer, "fstring_start({tag})")?;
        }
        TokenKind::FStringPart(text) => {
            push_fmt!(writer, "fstring_part({text:?})")?;
        }
        TokenKind::FStringEnd => {
            writer.push_str("fstring_end()")?;
        }
    };

    Ok(())
}
