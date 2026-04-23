use vx_jit_types::{
    LocalId, LocalInfo, StrictBinOp, StrictExpr, StrictFunction, StrictLiteral, StrictParam,
    StrictStmt, StrictType, StringOwnership,
};

use crate::{FragmentArg, FragmentResult, compile};

fn local(id: u32) -> LocalId {
    LocalId(id)
}

fn local_info(id: u32, ty: StrictType) -> LocalInfo {
    LocalInfo {
        id: local(id),
        ty,
        ownership: match ty {
            StrictType::String => StringOwnership::Borrowed,
            _ => StringOwnership::Static,
        },
    }
}

fn param(id: u32, name: &str, ty: StrictType) -> StrictParam {
    StrictParam {
        local: local(id),
        name: name.to_owned(),
        ty,
    }
}

// --- add(U64, U64) -> U64 ---

fn make_add_func() -> StrictFunction {
    // fn add(p0: U64, p1: U64) -> U64 { return p0 + p1 }
    StrictFunction {
        name: "add".to_owned(),
        params: vec![
            param(0, "lhs", StrictType::U64),
            param(1, "rhs", StrictType::U64),
        ],
        locals: vec![],
        return_type: StrictType::U64,
        return_ownership: StringOwnership::Static,
        body: vec![StrictStmt::Return(Some(StrictExpr::Binary {
            op: StrictBinOp::Add,
            lhs: Box::new(StrictExpr::Local(local(0))),
            rhs: Box::new(StrictExpr::Local(local(1))),
        }))],
    }
}

#[test]
fn test_u64_add() {
    let frag = compile(&make_add_func()).expect("add should compile");
    assert_eq!(
        frag.call(&[FragmentArg::U64(41), FragmentArg::U64(1)])
            .unwrap(),
        FragmentResult::U64(42)
    );
    assert_eq!(
        frag.call(&[FragmentArg::U64(0), FragmentArg::U64(0)])
            .unwrap(),
        FragmentResult::U64(0)
    );
    assert_eq!(
        frag.call(&[FragmentArg::U64(100), FragmentArg::U64(200)])
            .unwrap(),
        FragmentResult::U64(300)
    );
}

// --- sub(U64, U64) -> U64 ---

#[test]
fn test_u64_sub() {
    let func = StrictFunction {
        name: "sub".to_owned(),
        params: vec![
            param(0, "lhs", StrictType::U64),
            param(1, "rhs", StrictType::U64),
        ],
        locals: vec![],
        return_type: StrictType::U64,
        return_ownership: StringOwnership::Static,
        body: vec![StrictStmt::Return(Some(StrictExpr::Binary {
            op: StrictBinOp::Sub,
            lhs: Box::new(StrictExpr::Local(local(0))),
            rhs: Box::new(StrictExpr::Local(local(1))),
        }))],
    };
    let frag = compile(&func).expect("sub should compile");
    assert_eq!(
        frag.call(&[FragmentArg::U64(10), FragmentArg::U64(3)])
            .unwrap(),
        FragmentResult::U64(7)
    );
}

// --- eq(U64, U64) -> Bool ---

#[test]
fn test_eq() {
    let func = StrictFunction {
        name: "eq".to_owned(),
        params: vec![
            param(0, "a", StrictType::U64),
            param(1, "b", StrictType::U64),
        ],
        locals: vec![],
        return_type: StrictType::Bool,
        return_ownership: StringOwnership::Static,
        body: vec![StrictStmt::Return(Some(StrictExpr::Binary {
            op: StrictBinOp::Eq,
            lhs: Box::new(StrictExpr::Local(local(0))),
            rhs: Box::new(StrictExpr::Local(local(1))),
        }))],
    };
    let frag = compile(&func).expect("eq should compile");
    assert_eq!(
        frag.call(&[FragmentArg::U64(5), FragmentArg::U64(5)])
            .unwrap(),
        FragmentResult::Bool(true)
    );
    assert_eq!(
        frag.call(&[FragmentArg::U64(5), FragmentArg::U64(6)])
            .unwrap(),
        FragmentResult::Bool(false)
    );
}

// --- ne(U64, U64) -> Bool ---

#[test]
fn test_ne() {
    let func = StrictFunction {
        name: "ne".to_owned(),
        params: vec![
            param(0, "a", StrictType::U64),
            param(1, "b", StrictType::U64),
        ],
        locals: vec![],
        return_type: StrictType::Bool,
        return_ownership: StringOwnership::Static,
        body: vec![StrictStmt::Return(Some(StrictExpr::Binary {
            op: StrictBinOp::Ne,
            lhs: Box::new(StrictExpr::Local(local(0))),
            rhs: Box::new(StrictExpr::Local(local(1))),
        }))],
    };
    let frag = compile(&func).expect("ne should compile");
    assert_eq!(
        frag.call(&[FragmentArg::U64(5), FragmentArg::U64(6)])
            .unwrap(),
        FragmentResult::Bool(true)
    );
    assert_eq!(
        frag.call(&[FragmentArg::U64(5), FragmentArg::U64(5)])
            .unwrap(),
        FragmentResult::Bool(false)
    );
}

// --- const U64 ---

#[test]
fn test_const_u64() {
    let func = StrictFunction {
        name: "forty_two".to_owned(),
        params: vec![],
        locals: vec![],
        return_type: StrictType::U64,
        return_ownership: StringOwnership::Static,
        body: vec![StrictStmt::Return(Some(StrictExpr::Literal(
            StrictLiteral::U64(42),
        )))],
    };
    let frag = compile(&func).expect("const u64 should compile");
    assert_eq!(frag.call(&[]).unwrap(), FragmentResult::U64(42));
}

// --- const Bool ---

#[test]
fn test_const_bool() {
    let func = StrictFunction {
        name: "true_func".to_owned(),
        params: vec![],
        locals: vec![],
        return_type: StrictType::Bool,
        return_ownership: StringOwnership::Static,
        body: vec![StrictStmt::Return(Some(StrictExpr::Literal(
            StrictLiteral::Bool(true),
        )))],
    };
    let frag = compile(&func).expect("const bool should compile");
    assert_eq!(frag.call(&[]).unwrap(), FragmentResult::Bool(true));
}

#[test]
fn test_bool_identity() {
    let func = StrictFunction {
        name: "bool_identity".to_owned(),
        params: vec![param(0, "flag", StrictType::Bool)],
        locals: vec![],
        return_type: StrictType::Bool,
        return_ownership: StringOwnership::Static,
        body: vec![StrictStmt::Return(Some(StrictExpr::Local(local(0))))],
    };
    let frag = compile(&func).expect("bool identity should compile");
    assert_eq!(
        frag.call(&[FragmentArg::Bool(true)]).unwrap(),
        FragmentResult::Bool(true)
    );
    assert_eq!(
        frag.call(&[FragmentArg::Bool(false)]).unwrap(),
        FragmentResult::Bool(false)
    );
}

#[test]
fn test_bool_word_abi_in_mixed_signature() {
    let func = StrictFunction {
        name: "bool_middle_param".to_owned(),
        params: vec![
            param(0, "lhs", StrictType::U64),
            param(1, "flag", StrictType::Bool),
            param(2, "rhs", StrictType::U64),
        ],
        locals: vec![],
        return_type: StrictType::Bool,
        return_ownership: StringOwnership::Static,
        body: vec![StrictStmt::If {
            condition: StrictExpr::Local(local(1)),
            then_body: vec![StrictStmt::Return(Some(StrictExpr::Binary {
                op: StrictBinOp::Eq,
                lhs: Box::new(StrictExpr::Local(local(0))),
                rhs: Box::new(StrictExpr::Local(local(2))),
            }))],
            else_body: vec![StrictStmt::Return(Some(StrictExpr::Literal(
                StrictLiteral::Bool(false),
            )))],
        }],
    };
    let frag = compile(&func).expect("mixed bool ABI should compile");
    assert_eq!(
        frag.call(&[
            FragmentArg::U64(7),
            FragmentArg::Bool(true),
            FragmentArg::U64(7),
        ])
        .unwrap(),
        FragmentResult::Bool(true)
    );
    assert_eq!(
        frag.call(&[
            FragmentArg::U64(7),
            FragmentArg::Bool(true),
            FragmentArg::U64(8),
        ])
        .unwrap(),
        FragmentResult::Bool(false)
    );
    assert_eq!(
        frag.call(&[
            FragmentArg::U64(7),
            FragmentArg::Bool(false),
            FragmentArg::U64(7),
        ])
        .unwrap(),
        FragmentResult::Bool(false)
    );
}

// --- const String ---

#[test]
fn test_const_string() {
    let func = StrictFunction {
        name: "hello".to_owned(),
        params: vec![],
        locals: vec![],
        return_type: StrictType::String,
        return_ownership: StringOwnership::Static,
        body: vec![StrictStmt::Return(Some(StrictExpr::Literal(
            StrictLiteral::String("hello".to_owned()),
        )))],
    };
    let frag = compile(&func).expect("const string should compile");
    assert_eq!(
        frag.call(&[]).unwrap(),
        FragmentResult::String("hello".to_owned())
    );
}

// --- alias ---

#[test]
fn test_alias() {
    // fn alias(x: U64) -> U64 { let y = x; return y }
    let func = StrictFunction {
        name: "alias".to_owned(),
        params: vec![param(0, "x", StrictType::U64)],
        locals: vec![local_info(1, StrictType::U64)],
        return_type: StrictType::U64,
        return_ownership: StringOwnership::Static,
        body: vec![
            StrictStmt::Let {
                local: local(1),
                value: StrictExpr::Local(local(0)),
            },
            StrictStmt::Return(Some(StrictExpr::Local(local(1)))),
        ],
    };
    let frag = compile(&func).expect("alias should compile");
    assert_eq!(
        frag.call(&[FragmentArg::U64(99)]).unwrap(),
        FragmentResult::U64(99)
    );
}

// --- select (via If + Return) ---

#[test]
fn test_select_u64() {
    // fn pick(flag: Bool) -> U64 { if flag { 1 } else { 0 } }
    let func = StrictFunction {
        name: "pick".to_owned(),
        params: vec![param(0, "flag", StrictType::Bool)],
        locals: vec![],
        return_type: StrictType::U64,
        return_ownership: StringOwnership::Static,
        body: vec![StrictStmt::If {
            condition: StrictExpr::Local(local(0)),
            then_body: vec![StrictStmt::Return(Some(StrictExpr::Literal(
                StrictLiteral::U64(1),
            )))],
            else_body: vec![StrictStmt::Return(Some(StrictExpr::Literal(
                StrictLiteral::U64(0),
            )))],
        }],
    };
    let frag = compile(&func).expect("select u64 should compile");
    assert_eq!(
        frag.call(&[FragmentArg::Bool(true)]).unwrap(),
        FragmentResult::U64(1)
    );
    assert_eq!(
        frag.call(&[FragmentArg::Bool(false)]).unwrap(),
        FragmentResult::U64(0)
    );
}

// --- string_len ---

#[test]
fn test_string_len() {
    // fn str_len(s: String) -> U64 { return s.len }
    let func = StrictFunction {
        name: "str_len".to_owned(),
        params: vec![param(0, "s", StrictType::String)],
        locals: vec![],
        return_type: StrictType::U64,
        return_ownership: StringOwnership::Static,
        body: vec![StrictStmt::Return(Some(StrictExpr::StringLen {
            value: Box::new(StrictExpr::Local(local(0))),
        }))],
    };
    let frag = compile(&func).expect("str_len should compile");
    assert_eq!(
        frag.call(&[FragmentArg::String("hello".to_owned())])
            .unwrap(),
        FragmentResult::U64(5)
    );
    assert_eq!(
        frag.call(&[FragmentArg::String("".to_owned())]).unwrap(),
        FragmentResult::U64(0)
    );
}

// --- string_slice ---

#[test]
fn test_string_slice() {
    // fn str_slice(s: String, start: U64, end: U64) -> String { return s[start..end] }
    let func = StrictFunction {
        name: "str_slice".to_owned(),
        params: vec![
            param(0, "s", StrictType::String),
            param(1, "start", StrictType::U64),
            param(2, "end", StrictType::U64),
        ],
        locals: vec![],
        return_type: StrictType::String,
        return_ownership: StringOwnership::Borrowed,
        body: vec![StrictStmt::Return(Some(StrictExpr::StringSlice {
            value: Box::new(StrictExpr::Local(local(0))),
            start: Box::new(StrictExpr::Local(local(1))),
            end: Box::new(StrictExpr::Local(local(2))),
        }))],
    };
    let frag = compile(&func).expect("str_slice should compile");
    assert_eq!(
        frag.call(&[
            FragmentArg::String("serde".to_owned()),
            FragmentArg::U64(1),
            FragmentArg::U64(4),
        ])
        .unwrap(),
        FragmentResult::String("erd".to_owned())
    );
}

// --- string_concat ---

#[test]
fn test_string_concat() {
    // fn join(a: String, b: String) -> String { return concat(a, b) }
    let func = StrictFunction {
        name: "join".to_owned(),
        params: vec![
            param(0, "a", StrictType::String),
            param(1, "b", StrictType::String),
        ],
        locals: vec![],
        return_type: StrictType::String,
        return_ownership: StringOwnership::Owned,
        body: vec![StrictStmt::Return(Some(StrictExpr::ConcatStrings {
            parts: vec![StrictExpr::Local(local(0)), StrictExpr::Local(local(1))],
        }))],
    };
    let frag = compile(&func).expect("concat should compile");
    assert_eq!(
        frag.call(&[
            FragmentArg::String("hello".to_owned()),
            FragmentArg::String(" world".to_owned()),
        ])
        .unwrap(),
        FragmentResult::String("hello world".to_owned())
    );
}

// --- nested if (sparse_prefix-style) ---

#[test]
fn test_nested_if_select() {
    // fn classify(n: U64) -> U64 { if n == 1 { 10 } else { if n == 5 { 50 } else { 0 } } }
    let func = StrictFunction {
        name: "classify".to_owned(),
        params: vec![param(0, "n", StrictType::U64)],
        locals: vec![],
        return_type: StrictType::U64,
        return_ownership: StringOwnership::Static,
        body: vec![StrictStmt::If {
            condition: StrictExpr::Binary {
                op: StrictBinOp::Eq,
                lhs: Box::new(StrictExpr::Local(local(0))),
                rhs: Box::new(StrictExpr::Literal(StrictLiteral::U64(1))),
            },
            then_body: vec![StrictStmt::Return(Some(StrictExpr::Literal(
                StrictLiteral::U64(10),
            )))],
            else_body: vec![StrictStmt::If {
                condition: StrictExpr::Binary {
                    op: StrictBinOp::Eq,
                    lhs: Box::new(StrictExpr::Local(local(0))),
                    rhs: Box::new(StrictExpr::Literal(StrictLiteral::U64(5))),
                },
                then_body: vec![StrictStmt::Return(Some(StrictExpr::Literal(
                    StrictLiteral::U64(50),
                )))],
                else_body: vec![StrictStmt::Return(Some(StrictExpr::Literal(
                    StrictLiteral::U64(0),
                )))],
            }],
        }],
    };
    let frag = compile(&func).expect("nested if should compile");
    assert_eq!(
        frag.call(&[FragmentArg::U64(1)]).unwrap(),
        FragmentResult::U64(10)
    );
    assert_eq!(
        frag.call(&[FragmentArg::U64(5)]).unwrap(),
        FragmentResult::U64(50)
    );
    assert_eq!(
        frag.call(&[FragmentArg::U64(99)]).unwrap(),
        FragmentResult::U64(0)
    );
}

// --- smoke: string if-select returning different string literals ---

#[test]
fn test_string_select() {
    // fn pick(flag: Bool) -> String { if flag { "yes" } else { "no" } }
    let func = StrictFunction {
        name: "pick_str".to_owned(),
        params: vec![param(0, "flag", StrictType::Bool)],
        locals: vec![],
        return_type: StrictType::String,
        return_ownership: StringOwnership::Static,
        body: vec![StrictStmt::If {
            condition: StrictExpr::Local(local(0)),
            then_body: vec![StrictStmt::Return(Some(StrictExpr::Literal(
                StrictLiteral::String("yes".to_owned()),
            )))],
            else_body: vec![StrictStmt::Return(Some(StrictExpr::Literal(
                StrictLiteral::String("no".to_owned()),
            )))],
        }],
    };
    let frag = compile(&func).expect("string select should compile");
    assert_eq!(
        frag.call(&[FragmentArg::Bool(true)]).unwrap(),
        FragmentResult::String("yes".to_owned())
    );
    assert_eq!(
        frag.call(&[FragmentArg::Bool(false)]).unwrap(),
        FragmentResult::String("no".to_owned())
    );
}
