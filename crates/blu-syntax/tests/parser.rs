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
                .slice(
                    parsed
                        .ast()
                        .expression(local.value().unwrap())
                        .unwrap()
                        .span(),
                )
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
fn local_without_initializer_has_no_value_and_ends_at_its_name() {
    for profile in SemanticProfile::ALL {
        let source = source(b"local missing\nreturn missing".to_vec());
        let parsed = accepted(&source, profile, ParseLimits::default());
        let Statement::Local(local) = parsed.ast().statements()[0] else {
            panic!("expected local statement");
        };
        assert_eq!(source.slice(local.span()).unwrap(), b"local missing");
        assert_eq!(source.slice(local.name().span()).unwrap(), b"missing");
        assert_eq!(local.value(), None);
    }
}

#[test]
fn local_name_and_value_lists_preserve_order_and_adjustment_shape() {
    for profile in SemanticProfile::ALL {
        let source = source(b"local first, second, missing = 40, 2\nreturn first".to_vec());
        let parsed = accepted(&source, profile, ParseLimits::default());
        let Statement::LocalList(local) = &parsed.ast().statements()[0] else {
            panic!("expected local-list statement");
        };
        assert_eq!(local.names().len(), 3);
        assert_eq!(local.values().len(), 2);
        assert_eq!(source.slice(local.names()[0].span()).unwrap(), b"first");
        assert_eq!(source.slice(local.names()[1].span()).unwrap(), b"second");
        assert_eq!(source.slice(local.names()[2].span()).unwrap(), b"missing");
        assert_eq!(
            source.slice(local.span()).unwrap(),
            b"local first, second, missing = 40, 2"
        );
    }
}

#[test]
fn identifier_assignment_preserves_target_value_and_full_span() {
    for profile in SemanticProfile::ALL {
        let source = source(b"local answer = 40\nanswer = answer + 2\nreturn answer".to_vec());
        let parsed = accepted(&source, profile, ParseLimits::default());
        let Statement::Assignment(assignment) = parsed.ast().statements()[1] else {
            panic!("expected assignment statement");
        };
        assert_eq!(source.slice(assignment.target().span()).unwrap(), b"answer");
        assert_eq!(
            source
                .slice(parsed.ast().expression(assignment.value()).unwrap().span(),)
                .unwrap(),
            b"answer + 2"
        );
        assert_eq!(
            source.slice(assignment.span()).unwrap(),
            b"answer = answer + 2"
        );
    }
}

#[test]
fn assignment_target_and_value_lists_preserve_order_and_full_span() {
    for profile in SemanticProfile::ALL {
        let source = source(
            b"local first, second = 1, 2\nfirst, second = second, first\nreturn first".to_vec(),
        );
        let parsed = accepted(&source, profile, ParseLimits::default());
        let Statement::AssignmentList(assignment) = &parsed.ast().statements()[1] else {
            panic!("expected assignment-list statement");
        };
        assert_eq!(assignment.targets().len(), 2);
        assert_eq!(assignment.values().len(), 2);
        assert_eq!(
            source.slice(assignment.targets()[0].span()).unwrap(),
            b"first"
        );
        assert_eq!(
            source.slice(assignment.targets()[1].span()).unwrap(),
            b"second"
        );
        assert_eq!(
            source.slice(assignment.span()).unwrap(),
            b"first, second = second, first"
        );
    }
}

#[test]
fn identifier_statement_without_equal_is_rejected_structurally() {
    let source = source(b"answer\nreturn answer".to_vec());
    let outcome = parse(&source, SemanticProfile::Blu, ParseLimits::default()).unwrap();
    let rejected = outcome.rejected().unwrap();
    assert_eq!(rejected.diagnostics()[0].code().as_str(), "BLU-PARSE-0006");
    assert_eq!(
        rejected.diagnostics()[0].primary().span(),
        source.span(7, 13).unwrap()
    );
}

#[test]
fn semicolons_separate_statements_represent_empty_statements_and_trail_return() {
    for profile in SemanticProfile::ALL {
        let source = source(b";;local answer = 40;answer = answer + 2;;return answer;;;".to_vec());
        let parsed = accepted(&source, profile, ParseLimits::default());
        assert_eq!(parsed.ast().statements().len(), 3, "{profile}");
        assert!(matches!(parsed.ast().statements()[0], Statement::Local(_)));
        assert!(matches!(
            parsed.ast().statements()[1],
            Statement::Assignment(_)
        ));
        assert!(matches!(parsed.ast().statements()[2], Statement::Return(_)));
        assert_eq!(
            parsed
                .tokens()
                .iter()
                .filter(|token| token.kind() == TokenKind::Semicolon)
                .count(),
            8,
            "{profile}"
        );
    }

    let source = source(b"return;".to_vec());
    let parsed = accepted(&source, SemanticProfile::Blu, ParseLimits::default());
    let Statement::Return(statement) = &parsed.ast().statements()[0] else {
        panic!("expected return statement");
    };
    assert!(statement.values().is_empty());
    assert_eq!(source.slice(statement.span()).unwrap(), b"return");
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
fn subtraction_shares_addition_precedence_and_is_left_associative() {
    let source = source(b"return a + b - c - d + e".to_vec());
    let parsed = accepted(&source, SemanticProfile::Blu, ParseLimits::default());
    let Statement::Return(statement) = &parsed.ast().statements()[0] else {
        panic!("expected return statement");
    };
    let root = binary(&parsed, statement.values()[0]);
    assert_eq!(root.operator(), BinaryOperator::Add);
    let second_subtract = binary(&parsed, root.left());
    assert_eq!(second_subtract.operator(), BinaryOperator::Subtract);
    let first_subtract = binary(&parsed, second_subtract.left());
    assert_eq!(first_subtract.operator(), BinaryOperator::Subtract);
    assert_eq!(
        binary(&parsed, first_subtract.left()).operator(),
        BinaryOperator::Add
    );
    assert_eq!(source.slice(first_subtract.operator_span()).unwrap(), b"-");
}

#[test]
fn multiplication_binds_above_addition_and_is_left_associative() {
    let source = source(b"return a + b * c * d + e".to_vec());
    let parsed = accepted(&source, SemanticProfile::Blu, ParseLimits::default());
    let Statement::Return(statement) = &parsed.ast().statements()[0] else {
        panic!("expected return statement");
    };
    let root = binary(&parsed, statement.values()[0]);
    assert_eq!(root.operator(), BinaryOperator::Add);
    let first_add = binary(&parsed, root.left());
    assert_eq!(first_add.operator(), BinaryOperator::Add);
    let second_multiply = binary(&parsed, first_add.right());
    assert_eq!(second_multiply.operator(), BinaryOperator::Multiply);
    assert_eq!(
        binary(&parsed, second_multiply.left()).operator(),
        BinaryOperator::Multiply
    );
    assert_eq!(source.slice(second_multiply.operator_span()).unwrap(), b"*");
}

#[test]
fn division_shares_multiplication_precedence_and_is_left_associative() {
    let source = source(b"return a + b * c / d * e".to_vec());
    let parsed = accepted(&source, SemanticProfile::Blu, ParseLimits::default());
    let Statement::Return(statement) = &parsed.ast().statements()[0] else {
        panic!("expected return statement");
    };
    let root = binary(&parsed, statement.values()[0]);
    assert_eq!(root.operator(), BinaryOperator::Add);
    let last_multiply = binary(&parsed, root.right());
    assert_eq!(last_multiply.operator(), BinaryOperator::Multiply);
    let divide = binary(&parsed, last_multiply.left());
    assert_eq!(divide.operator(), BinaryOperator::Divide);
    assert_eq!(
        binary(&parsed, divide.left()).operator(),
        BinaryOperator::Multiply
    );
    assert_eq!(source.slice(divide.operator_span()).unwrap(), b"/");
}

#[test]
fn modulo_shares_multiplication_precedence_and_is_left_associative() {
    let source = source(b"return a * b % c / d".to_vec());
    let parsed = accepted(&source, SemanticProfile::Blu, ParseLimits::default());
    let Statement::Return(statement) = &parsed.ast().statements()[0] else {
        panic!("expected return statement");
    };
    let divide = binary(&parsed, statement.values()[0]);
    assert_eq!(divide.operator(), BinaryOperator::Divide);
    let modulo = binary(&parsed, divide.left());
    assert_eq!(modulo.operator(), BinaryOperator::Modulo);
    assert_eq!(
        binary(&parsed, modulo.left()).operator(),
        BinaryOperator::Multiply
    );
    assert_eq!(source.slice(modulo.operator_span()).unwrap(), b"%");
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
fn unary_negation_binds_tighter_than_binary_operators_and_associates_right() {
    let source = source(b"return - -value * -2".to_vec());
    let parsed = accepted(&source, SemanticProfile::Blu, ParseLimits::default());
    let Statement::Return(statement) = &parsed.ast().statements()[0] else {
        panic!("expected return statement");
    };
    let multiply = binary(&parsed, statement.values()[0]);
    assert_eq!(multiply.operator(), BinaryOperator::Multiply);
    let outer = unary(&parsed, multiply.left());
    assert_eq!(outer.operator(), UnaryOperator::Negate);
    let inner = unary(&parsed, outer.operand());
    assert_eq!(inner.operator(), UnaryOperator::Negate);
    assert_eq!(
        unary(&parsed, multiply.right()).operator(),
        UnaryOperator::Negate
    );
    assert_eq!(source.slice(outer.operator_span()).unwrap(), b"-");
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
