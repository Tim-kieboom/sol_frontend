use std::str::FromStr;

use ast_model::{
    ArrayKind, ArrayType, NamedTuple, ReferenceType, SolType, Stub, Tuple, TupleKind, TypeId,
};
use sol_tokenizer::model::{TokenKind, keyword::KeyWord, types::Types};
use sol_utils::{
    Ident, Mutable,
    collections::{
        array::RcArr,
        try_result::{
            ResultTryErr, ResultTryNotValue, ToResult, TryErr, TryError, TryNotValue, TryOk,
        },
    },
    fault::Fault,
    literal::{Number, TokenLiteral},
    sol_names::PrimitiveTypes,
};

use crate::{
    fault::{AstFault, AstResult, AstTryResult},
    parser::Parser,
    utils::{
        ARRAY, ARROW_LEFT, COLON, COMMA, DOT, MUT, NOT, OPTIONAL, POINTER, REF, ROUND_CLOSE,
        ROUND_OPEN, SQUARE_CLOSE, SQUARE_OPEN,
    },
};

impl<'a, 'f> Parser<'a, 'f> {
    pub(crate) fn try_parse_type(&mut self) -> AstTryResult<SolType, AstFault> {
        let begin = self.tokens.current_position();
        let result = self.inner_parse_type();
        if result.is_err() {
            self.goto(begin);
        }

        result
    }

    pub(crate) fn type_from_ident(&mut self, ident: Ident, generics: RcArr<TypeId>) -> SolType {
        if ident.as_str() == PrimitiveTypes::None.as_str() {
            self.bump();
            return SolType::None;
        };

        if let Ok(prim) = PrimitiveTypes::from_str(ident.as_str()) {
            return SolType::Primitive(prim);
        }

        SolType::Stub(Stub {
            name: ident.into_shared_str(),
            generics,
            occurrence: self.alloc_node(),
        })
    }

    fn parse_token_type(&mut self, type_val: Types) -> AstTryResult<SolType, AstFault> {
        self.bump();

        let prim = match type_val {
            Types::Res => return self.parse_res().try_err(),
            Types::RawPtr => return self.parse_raw_ptr().try_err(),

            Types::Any => return TryOk(SolType::Any),
            Types::None => return TryOk(SolType::None),
            Types::String => return TryOk(SolType::String),
            Types::FormatString => return TryOk(SolType::FormatString),
            Types::Error => return TryOk(SolType::Error),
            Types::Boolean => PrimitiveTypes::Boolean,
            Types::Int => PrimitiveTypes::Int,
            Types::Int8 => PrimitiveTypes::Int8,
            Types::Int16 => PrimitiveTypes::Int16,
            Types::Int32 => PrimitiveTypes::Int32,
            Types::Int64 => PrimitiveTypes::Int64,
            Types::Uint => PrimitiveTypes::Uint,
            Types::Uint8 => PrimitiveTypes::Uint8,
            Types::Uint16 => PrimitiveTypes::Uint16,
            Types::Uint32 => PrimitiveTypes::Uint32,
            Types::Uint64 => PrimitiveTypes::Uint64,
            Types::Float16 => PrimitiveTypes::Float16,
            Types::Float32 => PrimitiveTypes::Float32,
            Types::Float64 => PrimitiveTypes::Float64,
            Types::Char => PrimitiveTypes::Char,
            Types::Char8 => PrimitiveTypes::Char8,
            Types::Char16 => PrimitiveTypes::Char16,
            Types::Char32 => PrimitiveTypes::Char32,
            Types::Char64 => PrimitiveTypes::Char64,
            Types::CInt => PrimitiveTypes::CInt,
            Types::CUint => PrimitiveTypes::CUint,
            Types::CString => PrimitiveTypes::CStr,
        };
        TryOk(SolType::Primitive(prim))
    }

    fn parse_raw_ptr(&mut self) -> Result<SolType, crate::fault::AstFault> {
        let inner = if self.current_is(&ARROW_LEFT) {
            let mut generics = self.parse_generic_define().merge_to_result()?;

            let Some(inner) = generics.pop() else {
                return Err(Fault::error_with_kind(
                    crate::fault::AstErrorKind::RawPtrExpectsOneGeneric,
                    Some(self.token().span),
                ));
            };

            Some(inner)
        } else {
            None
        };

        Ok(SolType::RawPtr(inner))
    }

    fn parse_res(&mut self) -> Result<SolType, crate::fault::AstFault> {
        if self.current_is(&ARROW_LEFT) {
            let mut generics = self.parse_generic_define().merge_to_result()?;

            if generics.len() > 2 {
                return Err(Fault::error_with_kind(
                    crate::fault::AstErrorKind::ResExpectsAtMostTwoGenerics,
                    Some(self.token().span),
                ));
            }

            let err = if generics.len() == 2 {
                Some(generics.remove(1))
            } else {
                None
            };
            let ok = if generics.len() == 1 {
                Some(generics.remove(0))
            } else {
                None
            };

            Ok(SolType::Res { ok, err })
        } else {
            Ok(SolType::Res {
                ok: None,
                err: None,
            })
        }
    }

    fn inner_parse_type(&mut self) -> AstTryResult<SolType, AstFault> {
        let wrapper = self.get_type_wrapper()?;
        let mut ty = match self.get_base_type() {
            Ok(ty) => ty,
            Err(TryError::IsNotValue(_)) if !wrapper.is_empty() => {
                return TryErr(Fault::error_with_kind(
                    crate::fault::AstErrorKind::ArrayMissingElementType,
                    Some(self.token().span),
                ));
            }
            Err(e) => return Err(e),
        };

        for wrap in wrapper {
            let inner = self.intern_type(ty);
            ty = match wrap {
                ParseWrappers::ConstRef => {
                    SolType::Reference(ReferenceType::new(inner, Mutable::Immut))
                }
                ParseWrappers::MutRef => {
                    SolType::Reference(ReferenceType::new(inner, Mutable::Mut))
                }
                ParseWrappers::ConstPointer => {
                    SolType::Pointer(ReferenceType::new(inner, Mutable::Immut))
                }
                ParseWrappers::MutPointer => {
                    SolType::Pointer(ReferenceType::new(inner, Mutable::Mut))
                }
                ParseWrappers::Option => SolType::Optional(inner),
                ParseWrappers::Array(kind) => {
                    let array = ArrayType {
                        of_type: inner,
                        kind,
                    };
                    SolType::Array(array)
                }
            };
        }

        if self.current_is(&DOT) {
            let save = self.tokens.current_position();
            self.bump();
            if let Ok(variant) = self.try_bump_consume_ident() {
                let base = self.intern_type(ty);
                return TryOk(SolType::NamedVariant { base, variant });
            }
            self.goto(save);
        }

        Ok(ty)
    }

    fn get_base_type(&mut self) -> AstTryResult<SolType, AstFault> {
        const NONE_STR: &str = PrimitiveTypes::None.as_str();

        if self.current_is(&TokenKind::Keyword(KeyWord::Impl)) {
            self.bump();
            let inner = match self.try_parse_type() {
                Ok(val) => val,
                Err(TryError::IsErr(err)) => return TryErr(err),
                Err(TryError::IsNotValue(err)) => return TryNotValue(err),
            };
            let inner = self.intern_type(inner);
            return TryOk(SolType::ImplTrait(inner));
        }

        match &self.token().kind {
            TokenKind::Ident(val) if val == NONE_STR => {
                self.bump();
                return TryOk(SolType::None);
            }
            TokenKind::Types(type_val) => {
                return self.parse_token_type(*type_val);
            }
            &NOT => {
                self.bump();
                return TryOk(SolType::Never);
            }
            &ROUND_OPEN => {
                return self.parse_tuple_kind().map(SolType::TupleKind).try_err();
            }
            _ => (),
        };

        let ident = self.try_bump_consume_ident().try_not_value()?;

        if let Ok(keyword) = KeyWord::from_str(ident.as_str()) {
            return TryNotValue(Fault::error_with_kind(
                crate::fault::AstErrorKind::KeywordUsedAsType { keyword },
                Some(ident.span()),
            ));
        }

        if let Ok(prim) = PrimitiveTypes::from_str(ident.as_str()) {
            return TryOk(SolType::Primitive(prim));
        }

        let generics = if self.current_is(&ARROW_LEFT) {
            match self.parse_generic_define() {
                Ok(val) => val,
                Err(TryError::IsErr(err)) => return TryErr(err),
                Err(TryError::IsNotValue(err)) => {
                    return TryNotValue(err);
                }
            }
        } else {
            vec![]
        };

        TryOk(SolType::Stub(Stub {
            name: ident.into_shared_str(),
            generics: generics.into(),
            occurrence: self.alloc_node(),
        }))
    }

    fn get_type_wrapper(&mut self) -> AstTryResult<Vec<ParseWrappers>, AstFault> {
        let mut wrappers = vec![];
        loop {
            let possible_wrap = match self.token().kind {
                REF => {
                    if self.peek_is(&MUT) {
                        self.bump();
                        Some(ParseWrappers::MutRef)
                    } else {
                        Some(ParseWrappers::ConstRef)
                    }
                }
                POINTER => {
                    if self.peek_is(&MUT) {
                        self.bump();
                        Some(ParseWrappers::MutPointer)
                    } else {
                        Some(ParseWrappers::ConstPointer)
                    }
                }
                OPTIONAL => Some(ParseWrappers::Option),
                ARRAY => Some(ParseWrappers::Array(ArrayKind::HeapArray)),
                SQUARE_OPEN => Some(ParseWrappers::Array(self.get_array_type_wrapper()?)),
                _ => None,
            };

            let wrap = match possible_wrap {
                Some(val) => val,
                None => break,
            };

            self.bump();
            wrappers.push(wrap);
        }

        wrappers.reverse();
        TryOk(wrappers)
    }

    fn get_array_type_wrapper(&mut self) -> AstTryResult<ArrayKind, AstFault> {
        self.bump();

        let kind = if self.current_is_ident("_") {
            ArrayKind::StackArrayWildcard
        } else {
            match &self.token().kind {
                &REF => {
                    if matches!(self.peek().kind, TokenKind::Keyword(KeyWord::Mut)) {
                        self.bump();
                        ArrayKind::MutSlice
                    } else {
                        ArrayKind::ConstSlice
                    }
                }
                TokenKind::Literal(TokenLiteral::Number(Number::Uint(size))) => {
                    ArrayKind::StackArray(*size)
                }
                other => {
                    return TryNotValue(Fault::error_with_kind(
                        crate::fault::AstErrorKind::InvalidArrayTypeWrapperToken {
                            found: other.clone(),
                        },
                        Some(self.token().span),
                    ));
                }
            }
        };

        self.bump();
        if self.token().kind != SQUARE_CLOSE {
            return TryNotValue(self.get_expect_error(&SQUARE_CLOSE));
        }

        Ok(kind)
    }

    fn parse_tuple_kind(&mut self) -> AstResult<TupleKind> {
        self.expect(&ROUND_OPEN)?;
        self.skip_end_lines();
        if self.peek_is(&COLON) {
            return self.parse_named_tuple().map(TupleKind::NamedTuple);
        }

        self.parse_tuple().map(TupleKind::Tuple)
    }

    fn parse_named_tuple(&mut self) -> AstResult<NamedTuple> {
        let mut values = vec![];
        loop {
            let ident = self.try_bump_consume_ident()?;
            self.expect(&COLON)?;
            let ty = self.try_parse_type().merge_to_result()?;
            let ty = self.intern_type(ty);
            values.push((ident, ty));

            self.skip_end_lines();
            if !self.current_is(&COMMA) {
                break;
            }
            self.bump();
        }

        self.expect(&ROUND_CLOSE)?;
        Ok(values.into())
    }

    fn parse_tuple(&mut self) -> AstResult<Tuple> {
        let mut values = vec![];
        loop {
            let ty = self.try_parse_type().merge_to_result()?;
            let ty = self.intern_type(ty);
            values.push(ty);

            self.skip_end_lines();
            if !self.current_is(&COMMA) {
                break;
            }
            self.bump();
        }

        self.expect(&ROUND_CLOSE)?;
        Ok(values.into())
    }
}

enum ParseWrappers {
    ConstRef,
    MutRef,
    ConstPointer,
    MutPointer,
    Option,
    Array(ArrayKind),
}
