use blu_core::{
    DiagnosticError, DiagnosticLimit, DiagnosticLimits, Phase, SemanticProfile, SourceFile,
    SourceId, SourceLimits,
};
use blu_syntax::{
    BinaryOperator, ExpressionId, ExpressionKind, ParseError, ParseLimit, ParseLimits,
    ParseOutcome, Statement, TokenKind, UnaryOperator, parse,
};

fn source(bytes: impl Into<Vec<u8>>) -> SourceFile {
    SourceFile::new(
        SourceId::new(29),
        "parser-test.lua",
        bytes,
        SourceLimits::default(),
    )
    .unwrap()
}

fn accepted(
    source: &SourceFile,
    profile: SemanticProfile,
    limits: ParseLimits,
) -> blu_syntax::Parsed {
    match parse(source, profile, limits).unwrap() {
        ParseOutcome::Accepted(parsed) => parsed,
        ParseOutcome::Rejected(rejected) => {
            panic!("unexpected rejection: {:?}", rejected.diagnostics())
        }
    }
}

fn binary(parsed: &blu_syntax::Parsed, id: ExpressionId) -> blu_syntax::BinaryExpression {
    match parsed.ast().expression(id).unwrap().kind() {
        ExpressionKind::Binary(binary) => binary,
        other => panic!("expected binary expression, found {other:?}"),
    }
}

fn unary(parsed: &blu_syntax::Parsed, id: ExpressionId) -> blu_syntax::UnaryExpression {
    match parsed.ast().expression(id).unwrap().kind() {
        ExpressionKind::Unary(unary) => unary,
        other => panic!("expected unary expression, found {other:?}"),
    }
}

#[test]
fn vertical_slice_preserves_profile_spans_and_trivia_for_all_profiles() {
    for profile in SemanticProfile::ALL {
        let bytes = format!(
            "--!dialect {}\n-- retained\nlocal answer = 40\nreturn answer + 2",
            profile.as_str()
        );
        let source = source(bytes.into_bytes());
        let parsed = accepted(&source, profile, ParseLimits::default());

        assert_eq!(parsed.profile(), profile);
        assert_eq!(parsed.ast().profile(), profile);
        assert_eq!(parsed.directive().unwrap().profile(), profile);
        assert_eq!(parsed.ast().statements().len(), 2);
        assert!(parsed.tokens().iter().any(|token| {
            token.kind() == TokenKind::Comment
                && source.slice(token.span()).unwrap() == b"-- retained"
        }));

        let Statement::Local(local) = &parsed.ast().statements()[0] else {
            panic!("expected local statement");
        };
        assert_eq!(source.slice(local.name().span()).unwrap(), b"answer");
        assert_eq!(
            source
                .slice(parsed.ast().expression(local.value()).unwrap().span())
                .unwrap(),
            b"40"
        );
        let Statement::Return(return_statement) = &parsed.ast().statements()[1] else {
            panic!("expected return statement");
        };
        assert_eq!(return_statement.values().len(), 1);
        assert_eq!(
            source.slice(return_statement.span()).unwrap(),
            b"return answer + 2"
        );
    }
}

#[test]
fn floor_divide_binds_tighter_than_add_and_both_are_left_associative() {
    let source = source(b"return a + b // c // d + e".to_vec());
    let parsed = accepted(&source, SemanticProfile::Lua54, ParseLimits::default());
    let Statement::Return(statement) = &parsed.ast().statements()[0] else {
        panic!("expected return statement");
    };

    let root = binary(&parsed, statement.values()[0]);
    assert_eq!(root.operator(), BinaryOperator::Add);
    assert_eq!(
        source
            .slice(parsed.ast().expression(root.right()).unwrap().span())
            .unwrap(),
        b"e"
    );
    let first_add = binary(&parsed, root.left());
    assert_eq!(first_add.operator(), BinaryOperator::Add);
    assert_eq!(
        source
            .slice(parsed.ast().expression(first_add.left()).unwrap().span())
            .unwrap(),
        b"a"
    );
    let second_divide = binary(&parsed, first_add.right());
    assert_eq!(second_divide.operator(), BinaryOperator::FloorDivide);
    assert_eq!(source.slice(second_divide.operator_span()).unwrap(), b"//");
    let first_divide = binary(&parsed, second_divide.left());
    assert_eq!(first_divide.operator(), BinaryOperator::FloorDivide);
    assert_eq!(
        source
            .slice(parsed.ast().expression(first_divide.left()).unwrap().span())
            .unwrap(),
        b"b"
    );
    assert_eq!(
        source
            .slice(
                parsed
                    .ast()
                    .expression(first_divide.right())
                    .unwrap()
                    .span()
            )
            .unwrap(),
        b"c"
    );
}

#[test]
fn grouping_parentheses_override_precedence_and_retain_their_span() {
    let source = source(b"return (a + b) // c".to_vec());
    let parsed = accepted(&source, SemanticProfile::Lua54, ParseLimits::default());
    let Statement::Return(statement) = &parsed.ast().statements()[0] else {
        panic!("expected return statement");
    };
    let root = binary(&parsed, statement.values()[0]);
    assert_eq!(root.operator(), BinaryOperator::FloorDivide);
    let group = parsed.ast().expression(root.left()).unwrap();
    let ExpressionKind::Group(inner) = group.kind() else {
        panic!("expected grouped expression");
    };
    assert_eq!(source.slice(group.span()).unwrap(), b"(a + b)");
    assert_eq!(binary(&parsed, inner).operator(), BinaryOperator::Add);
}

#[test]
fn unary_not_binds_tighter_than_binary_operators_and_associates_right() {
    let source = source(b"return not not false + 1".to_vec());
    let parsed = accepted(&source, SemanticProfile::Blu, ParseLimits::default());
    let Statement::Return(statement) = &parsed.ast().statements()[0] else {
        panic!("expected return statement");
    };
    let add = binary(&parsed, statement.values()[0]);
    let outer = unary(&parsed, add.left());
    assert_eq!(outer.operator(), UnaryOperator::Not);
    assert_eq!(source.slice(outer.operator_span()).unwrap(), b"not");
    let inner = unary(&parsed, outer.operand());
    assert_eq!(inner.operator(), UnaryOperator::Not);
    assert_eq!(
        source
            .slice(parsed.ast().expression(inner.operand()).unwrap().span())
            .unwrap(),
        b"false"
    );
}

#[test]
fn missing_group_closer_is_a_structured_rejection() {
    let source = source(b"return (1 + 2".to_vec());
    let ParseOutcome::Rejected(rejected) =
        parse(&source, SemanticProfile::Blu, ParseLimits::default()).unwrap()
    else {
        panic!("missing group closer should reject");
    };
    assert_eq!(rejected.diagnostics()[0].code().as_str(), "BLU-PARSE-0006");
    assert_eq!(rejected.diagnostics()[0].phase(), Phase::Parse);
}

#[test]
fn nil_and_boolean_literals_have_distinct_ast_kinds() {
    let source = source(b"return nil, true, false".to_vec());
    let parsed = accepted(&source, SemanticProfile::Blu, ParseLimits::default());
    let Statement::Return(statement) = &parsed.ast().statements()[0] else {
        panic!("expected return statement");
    };
    let kinds: Vec<_> = statement
        .values()
        .iter()
        .map(|id| parsed.ast().expression(*id).unwrap().kind())
        .collect();
    assert_eq!(
        kinds,
        [
            ExpressionKind::Nil,
            ExpressionKind::Boolean(true),
            ExpressionKind::Boolean(false),
        ]
    );
}

#[test]
fn quoted_string_literal_retains_delimiters_in_its_ast_span() {
    let source = source(b"return 'blu'".to_vec());
    let parsed = accepted(&source, SemanticProfile::Blu, ParseLimits::default());
    let Statement::Return(statement) = &parsed.ast().statements()[0] else {
        panic!("expected return statement");
    };
    let expression = parsed.ast().expression(statement.values()[0]).unwrap();
    assert_eq!(expression.kind(), ExpressionKind::StringLiteral);
    assert_eq!(source.slice(expression.span()).unwrap(), b"'blu'");
}

#[test]
fn return_expression_lists_are_spanned_and_comma_separated() {
    let source = source(b"return 1, value + 2, 9 // 4".to_vec());
    let parsed = accepted(&source, SemanticProfile::Lua53, ParseLimits::default());
    let Statement::Return(statement) = &parsed.ast().statements()[0] else {
        panic!("expected return statement");
    };

    assert_eq!(statement.values().len(), 3);
    assert_eq!(
        source.slice(statement.span()).unwrap(),
        b"return 1, value + 2, 9 // 4"
    );
    assert_eq!(
        parsed
            .tokens()
            .iter()
            .filter(|token| token.kind() == TokenKind::Comma)
            .count(),
        2
    );
}

#[test]
fn bare_return_has_an_empty_value_list_and_keyword_span() {
    for bytes in [b"return".as_slice(), b"return -- done".as_slice()] {
        let source = source(bytes.to_vec());
        let parsed = accepted(&source, SemanticProfile::Blu, ParseLimits::default());
        let Statement::Return(statement) = &parsed.ast().statements()[0] else {
            panic!("expected return statement");
        };
        assert!(statement.values().is_empty());
        assert_eq!(source.slice(statement.span()).unwrap(), b"return");
    }
}

#[test]
fn malformed_input_is_a_structural_rejection_with_stable_diagnostics() {
    let source = source(b"local = 1\nreturn value +".to_vec());
    let outcome = parse(&source, SemanticProfile::Blu, ParseLimits::default()).unwrap();
    assert!(outcome.accepted().is_none());
    let rejected = outcome.rejected().unwrap();

    assert_eq!(rejected.profile(), SemanticProfile::Blu);
    assert_eq!(rejected.diagnostics().len(), 2);
    assert_eq!(rejected.diagnostics()[0].code().as_str(), "BLU-PARSE-0002");
    assert_eq!(rejected.diagnostics()[0].phase(), Phase::Parse);
    assert_eq!(rejected.diagnostics()[0].expected(), ["identifier"]);
    assert_eq!(
        source
            .slice(rejected.diagnostics()[0].primary().span())
            .unwrap(),
        b"="
    );
    assert_eq!(rejected.diagnostics()[1].code().as_str(), "BLU-PARSE-0004");
    assert!(rejected.diagnostics()[1].primary().span().is_empty());
    assert_eq!(
        rejected.diagnostics()[1]
            .primary()
            .span()
            .start()
            .as_usize(),
        source.len()
    );
}

#[test]
fn empty_trivia_only_and_truncated_inputs_do_not_panic() {
    for bytes in [b"".as_slice(), b" \n-- retained".as_slice()] {
        let source = source(bytes.to_vec());
        let parsed = accepted(&source, SemanticProfile::Blu, ParseLimits::default());

        assert!(parsed.ast().statements().is_empty());
        assert!(parsed.ast().expressions().is_empty());
        assert!(parsed.ast().span().is_empty());
    }

    let source = source(b"return 1 +".to_vec());
    let outcome = parse(&source, SemanticProfile::Blu, ParseLimits::default()).unwrap();
    let rejected = outcome.rejected().unwrap();
    assert_eq!(rejected.diagnostics().len(), 1);
    assert_eq!(rejected.diagnostics()[0].code().as_str(), "BLU-PARSE-0004");
    assert!(rejected.diagnostics()[0].primary().span().is_empty());
    assert_eq!(
        rejected.diagnostics()[0]
            .primary()
            .span()
            .start()
            .as_usize(),
        source.len()
    );
}

#[test]
fn ast_depth_and_diagnostic_limits_are_structured() {
    let valid_source = source(b"return 1 + 2 + 3".to_vec());
    assert_eq!(
        parse(
            &valid_source,
            SemanticProfile::Blu,
            ParseLimits {
                max_ast_nodes: 3,
                ..ParseLimits::default()
            },
        ),
        Err(ParseError::Limit {
            kind: ParseLimit::AstNodes,
            required: 4,
            limit: 3,
        })
    );
    assert_eq!(
        parse(
            &valid_source,
            SemanticProfile::Blu,
            ParseLimits {
                max_expression_depth: 2,
                ..ParseLimits::default()
            },
        ),
        Err(ParseError::Limit {
            kind: ParseLimit::ExpressionDepth,
            required: 3,
            limit: 2,
        })
    );

    let malformed = source(b"1 2".to_vec());
    assert_eq!(
        parse(
            &malformed,
            SemanticProfile::Blu,
            ParseLimits {
                max_diagnostics: 1,
                ..ParseLimits::default()
            },
        ),
        Err(ParseError::Limit {
            kind: ParseLimit::Diagnostics,
            required: 2,
            limit: 1,
        })
    );
}

#[test]
fn diagnostic_value_limits_map_through_parse_error() {
    let source = source(b"return 1 +".to_vec());
    assert!(matches!(
        parse(
            &source,
            SemanticProfile::Blu,
            ParseLimits {
                lexer: blu_syntax::LexerLimits {
                    diagnostic_limits: DiagnosticLimits {
                        max_expected_items: 0,
                        ..DiagnosticLimits::default()
                    },
                    ..blu_syntax::LexerLimits::default()
                },
                ..ParseLimits::default()
            },
        ),
        Err(ParseError::Diagnostic(DiagnosticError::Limit {
            kind: DiagnosticLimit::ExpectedItems,
            required: 1,
            limit: 0,
        }))
    ));
}

#[test]
fn lua51_and_lua52_floor_divide_rejection_is_inherited_from_lexing() {
    for profile in [SemanticProfile::Lua51, SemanticProfile::Lua52] {
        let source = source(b"return 7 // 2".to_vec());
        let outcome = parse(&source, profile, ParseLimits::default()).unwrap();
        let rejected = outcome.rejected().unwrap();

        assert!(rejected.lexed().has_errors(), "{profile}");
        assert_eq!(rejected.diagnostics().len(), 1, "{profile}");
        assert_eq!(rejected.diagnostics()[0].code().as_str(), "BLU-LEX-0002");
        assert_eq!(rejected.diagnostics()[0].phase(), Phase::Lex);
        assert_eq!(rejected.diagnostics()[0].profile(), profile);
        assert_eq!(
            source
                .slice(rejected.diagnostics()[0].primary().span())
                .unwrap(),
            b"//"
        );
    }
}
