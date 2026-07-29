use blu_core::{
    DiagnosticError, DiagnosticLimit, DiagnosticLimits, SemanticProfile, SourceFile, SourceId,
    SourceLimits,
};
use blu_syntax::{LexError, LexerLimit, LexerLimits, TokenKind, lex};

fn source(bytes: impl Into<Vec<u8>>) -> SourceFile {
    SourceFile::new(
        SourceId::new(17),
        "lexer-test.lua",
        bytes,
        SourceLimits::default(),
    )
    .unwrap()
}

fn significant_kinds(lexed: &blu_syntax::Lexed) -> Vec<TokenKind> {
    lexed
        .tokens()
        .iter()
        .map(|token| token.kind())
        .filter(|kind| !matches!(kind, TokenKind::Whitespace | TokenKind::Comment))
        .collect()
}

#[test]
fn vertical_slice_tokens_keep_half_open_byte_spans() {
    let source = source(b"--!dialect lua54\nlocal answer = 40\nreturn answer + 2".to_vec());
    let lexed = lex(&source, SemanticProfile::Lua54, LexerLimits::default()).unwrap();

    assert!(!lexed.has_errors());
    assert_eq!(lexed.profile(), SemanticProfile::Lua54);
    let directive = lexed.directive().unwrap();
    assert_eq!(directive.profile(), SemanticProfile::Lua54);
    assert_eq!(source.slice(directive.span()).unwrap(), b"--!dialect lua54");
    assert_eq!(source.slice(directive.value_span()).unwrap(), b"lua54");
    assert_eq!(
        significant_kinds(&lexed),
        [
            TokenKind::DialectDirective,
            TokenKind::Local,
            TokenKind::Identifier,
            TokenKind::Equal,
            TokenKind::DecimalInteger,
            TokenKind::Return,
            TokenKind::Identifier,
            TokenKind::Plus,
            TokenKind::DecimalInteger,
        ]
    );

    for token in lexed.tokens() {
        let span = token.span();
        assert!(span.start() <= span.end());
        assert!(span.end().as_usize() <= source.len());
    }
}

#[test]
fn floor_division_gate_covers_all_seven_profiles() {
    for profile in SemanticProfile::ALL {
        let source = source(b"return 7 // 2".to_vec());
        let lexed = lex(&source, profile, LexerLimits::default()).unwrap();
        assert!(
            significant_kinds(&lexed).contains(&TokenKind::FloorDivide),
            "{profile}"
        );
        let rejected = matches!(profile, SemanticProfile::Lua51 | SemanticProfile::Lua52);
        assert_eq!(lexed.has_errors(), rejected, "{profile}");
        if rejected {
            let diagnostic = &lexed.diagnostics()[0];
            assert_eq!(diagnostic.code().as_str(), "BLU-LEX-0002");
            assert_eq!(diagnostic.profile(), profile);
            assert_eq!(source.slice(diagnostic.primary().span()).unwrap(), b"//");
        }
    }
}

#[test]
fn grouping_parentheses_are_profile_neutral_tokens() {
    for profile in SemanticProfile::ALL {
        let source = source(b"return (1 + 2)".to_vec());
        let lexed = lex(&source, profile, LexerLimits::default()).unwrap();
        assert!(!lexed.has_errors(), "{profile}");
        assert_eq!(
            significant_kinds(&lexed),
            [
                TokenKind::Return,
                TokenKind::LeftParenthesis,
                TokenKind::DecimalInteger,
                TokenKind::Plus,
                TokenKind::DecimalInteger,
                TokenKind::RightParenthesis,
            ],
            "{profile}"
        );
    }
}

#[test]
fn binary_minus_is_a_profile_neutral_token() {
    for profile in SemanticProfile::ALL {
        let source = source(b"return 40 - 2".to_vec());
        let lexed = lex(&source, profile, LexerLimits::default()).unwrap();
        assert!(!lexed.has_errors(), "{profile}");
        assert_eq!(
            significant_kinds(&lexed),
            [
                TokenKind::Return,
                TokenKind::DecimalInteger,
                TokenKind::Minus,
                TokenKind::DecimalInteger,
            ],
            "{profile}"
        );
    }
}

#[test]
fn semicolon_is_a_profile_neutral_statement_token() {
    for profile in SemanticProfile::ALL {
        let source = source(b";local answer = 42;return answer;".to_vec());
        let lexed = lex(&source, profile, LexerLimits::default()).unwrap();
        assert!(!lexed.has_errors(), "{profile}");
        assert_eq!(
            significant_kinds(&lexed)
                .iter()
                .filter(|kind| **kind == TokenKind::Semicolon)
                .count(),
            3,
            "{profile}"
        );
    }
}

#[test]
fn multiplication_is_a_profile_neutral_token() {
    for profile in SemanticProfile::ALL {
        let source = source(b"return 6 * 7".to_vec());
        let lexed = lex(&source, profile, LexerLimits::default()).unwrap();
        assert!(!lexed.has_errors(), "{profile}");
        assert_eq!(
            significant_kinds(&lexed),
            [
                TokenKind::Return,
                TokenKind::DecimalInteger,
                TokenKind::Star,
                TokenKind::DecimalInteger,
            ],
            "{profile}"
        );
    }
}

#[test]
fn division_is_a_profile_neutral_token_distinct_from_floor_division() {
    for profile in SemanticProfile::ALL {
        let source = source(b"return 21 / 2".to_vec());
        let lexed = lex(&source, profile, LexerLimits::default()).unwrap();
        assert!(!lexed.has_errors(), "{profile}");
        assert_eq!(
            significant_kinds(&lexed),
            [
                TokenKind::Return,
                TokenKind::DecimalInteger,
                TokenKind::Slash,
                TokenKind::DecimalInteger,
            ],
            "{profile}"
        );
    }
}

#[test]
fn modulo_is_a_profile_neutral_token() {
    for profile in SemanticProfile::ALL {
        let source = source(b"return 7 % 3".to_vec());
        let lexed = lex(&source, profile, LexerLimits::default()).unwrap();
        assert!(!lexed.has_errors(), "{profile}");
        assert_eq!(
            significant_kinds(&lexed),
            [
                TokenKind::Return,
                TokenKind::DecimalInteger,
                TokenKind::Percent,
                TokenKind::DecimalInteger,
            ],
            "{profile}"
        );
    }
}

#[test]
fn exponentiation_is_a_profile_neutral_token() {
    for profile in SemanticProfile::ALL {
        let source = source(b"return 2 ^ 8".to_vec());
        let lexed = lex(&source, profile, LexerLimits::default()).unwrap();
        assert!(!lexed.has_errors(), "{profile}");
        assert_eq!(
            significant_kinds(&lexed),
            [
                TokenKind::Return,
                TokenKind::DecimalInteger,
                TokenKind::Caret,
                TokenKind::DecimalInteger,
            ],
            "{profile}"
        );
    }
}

#[test]
fn nil_and_boolean_literals_are_profile_neutral_keywords() {
    for profile in SemanticProfile::ALL {
        let source = source(b"return nil, true, false".to_vec());
        let lexed = lex(&source, profile, LexerLimits::default()).unwrap();
        assert!(!lexed.has_errors(), "{profile}");
        assert_eq!(
            significant_kinds(&lexed),
            [
                TokenKind::Return,
                TokenKind::Nil,
                TokenKind::Comma,
                TokenKind::True,
                TokenKind::Comma,
                TokenKind::False,
            ],
            "{profile}"
        );
    }
}

#[test]
fn unary_not_is_a_profile_neutral_keyword() {
    for profile in SemanticProfile::ALL {
        let source = source(b"return not false".to_vec());
        let lexed = lex(&source, profile, LexerLimits::default()).unwrap();
        assert!(!lexed.has_errors(), "{profile}");
        assert_eq!(
            significant_kinds(&lexed),
            [TokenKind::Return, TokenKind::Not, TokenKind::False],
            "{profile}"
        );
    }
}

#[test]
fn quoted_strings_and_common_escapes_are_profile_neutral_byte_tokens() {
    for profile in SemanticProfile::ALL {
        let source = source(br#"return 'blu', "lua", 'a\'b', "a\"b", "\\\a\b\f\n\r\t\v""#.to_vec());
        let lexed = lex(&source, profile, LexerLimits::default()).unwrap();
        assert!(!lexed.has_errors(), "{profile}");
        let strings: Vec<_> = lexed
            .tokens()
            .iter()
            .filter(|token| token.kind() == TokenKind::StringLiteral)
            .map(|token| source.slice(token.span()).unwrap())
            .collect();
        assert_eq!(
            strings,
            [
                b"'blu'".as_slice(),
                b"\"lua\"".as_slice(),
                br#"'a\'b'"#.as_slice(),
                br#""a\"b""#.as_slice(),
                br#""\\\a\b\f\n\r\t\v""#.as_slice(),
            ]
        );
    }
}

#[test]
fn profile_sensitive_escapes_and_unterminated_strings_fail_explicitly() {
    let escaped = source(br#"return "a\x41b""#.to_vec());
    let escaped = lex(&escaped, SemanticProfile::Lua54, LexerLimits::default()).unwrap();
    assert_eq!(escaped.diagnostics()[0].code().as_str(), "BLU-LEX-0007");
    assert_eq!(escaped.diagnostics()[0].found(), Some(b"\\".as_slice()));

    let unterminated = source(b"return 'blu".to_vec());
    let unterminated = lex(&unterminated, SemanticProfile::Luau, LexerLimits::default()).unwrap();
    assert_eq!(
        unterminated.diagnostics()[0].code().as_str(),
        "BLU-LEX-0008"
    );
}

#[test]
fn conflicting_directive_is_reported_on_its_value() {
    let source = source(b"--!dialect lua54\r\nreturn 1".to_vec());
    let lexed = lex(&source, SemanticProfile::Lua53, LexerLimits::default()).unwrap();

    assert_eq!(lexed.profile(), SemanticProfile::Lua53);
    assert_eq!(lexed.directive().unwrap().profile(), SemanticProfile::Lua54);
    assert_eq!(
        source.slice(lexed.directive().unwrap().span()).unwrap(),
        b"--!dialect lua54"
    );
    assert!(
        lexed
            .tokens()
            .iter()
            .any(|token| source.slice(token.span()).unwrap() == b"\r\n")
    );
    assert_eq!(lexed.diagnostics().len(), 1);
    let diagnostic = &lexed.diagnostics()[0];
    assert_eq!(diagnostic.code().as_str(), "BLU-LEX-0005");
    assert_eq!(diagnostic.profile(), SemanticProfile::Lua53);
    assert_eq!(source.slice(diagnostic.primary().span()).unwrap(), b"lua54");
    assert_eq!(diagnostic.primary().span().start().get(), 11);
    assert_eq!(diagnostic.primary().span().end().get(), 16);
}

#[test]
fn unknown_and_non_utf8_bytes_have_deterministic_raw_diagnostics() {
    let source = source(vec![b'@', 0xff, b'+']);
    let first = lex(&source, SemanticProfile::Blu, LexerLimits::default()).unwrap();
    let second = lex(&source, SemanticProfile::Blu, LexerLimits::default()).unwrap();

    assert_eq!(first, second);
    assert_eq!(
        significant_kinds(&first),
        [TokenKind::Unknown, TokenKind::Unknown, TokenKind::Plus]
    );
    assert_eq!(first.diagnostics().len(), 2);
    assert_eq!(first.diagnostics()[0].found(), Some(&b"@"[..]));
    assert_eq!(first.diagnostics()[1].found(), Some(&[0xff][..]));
    assert_eq!(first.diagnostics()[1].primary().span().start().get(), 1);
}

#[test]
fn unknown_non_utf8_directive_profile_preserves_raw_bytes() {
    let source = source(b"--!dialect \xff\r\nreturn 1".to_vec());
    let lexed = lex(&source, SemanticProfile::Blu, LexerLimits::default()).unwrap();

    assert_eq!(lexed.directive(), None);
    assert_eq!(lexed.diagnostics().len(), 1);
    assert_eq!(lexed.diagnostics()[0].code().as_str(), "BLU-LEX-0004");
    assert_eq!(lexed.diagnostics()[0].found(), Some(&[0xff][..]));
}

#[test]
fn crlf_is_one_retained_whitespace_token_with_byte_spans() {
    let source = source(b"local x\r\nreturn x".to_vec());
    let lexed = lex(&source, SemanticProfile::Lua51, LexerLimits::default()).unwrap();
    let crlf = lexed
        .tokens()
        .iter()
        .find(|token| source.slice(token.span()).unwrap() == b"\r\n")
        .unwrap();

    assert_eq!(crlf.kind(), TokenKind::Whitespace);
    assert_eq!(crlf.span().start().get(), 7);
    assert_eq!(crlf.span().end().get(), 9);
    assert_eq!(
        source.position(crlf.span().end().as_usize()).unwrap().line,
        1
    );
}

#[test]
fn line_and_multiline_comments_are_retained_without_inner_tokens() {
    let source = source(b"-- line\r\n--[=[ long\ncomment ]=]\nreturn 1".to_vec());
    let lexed = lex(&source, SemanticProfile::Lua55, LexerLimits::default()).unwrap();
    let comments: Vec<_> = lexed
        .tokens()
        .iter()
        .filter(|token| token.kind() == TokenKind::Comment)
        .map(|token| source.slice(token.span()).unwrap())
        .collect();

    assert_eq!(
        comments,
        [b"-- line".as_slice(), b"--[=[ long\ncomment ]=]"]
    );
    assert!(!lexed.has_errors());
}

#[test]
fn truncated_directive_and_long_comment_are_diagnosed() {
    let directive_source = source(b"--!dialect".to_vec());
    let directive = lex(
        &directive_source,
        SemanticProfile::Blu,
        LexerLimits::default(),
    )
    .unwrap();
    assert_eq!(directive.diagnostics()[0].code().as_str(), "BLU-LEX-0003");
    assert!(directive.diagnostics()[0].primary().span().is_empty());
    assert_eq!(
        directive.diagnostics()[0].primary().span().start().get(),
        10
    );

    let comment_source = source(b"--[=[ never closed".to_vec());
    let comment = lex(
        &comment_source,
        SemanticProfile::Lua52,
        LexerLimits::default(),
    )
    .unwrap();
    assert_eq!(comment.diagnostics()[0].code().as_str(), "BLU-LEX-0006");
    assert_eq!(
        comment_source
            .slice(comment.diagnostics()[0].primary().span())
            .unwrap(),
        b"[=["
    );
}

#[test]
fn token_and_diagnostic_limits_fail_before_unbounded_growth() {
    let token_source = source(b"a b".to_vec());
    assert_eq!(
        lex(
            &token_source,
            SemanticProfile::Blu,
            LexerLimits {
                max_tokens: 2,
                ..LexerLimits::default()
            },
        ),
        Err(LexError::Limit {
            kind: LexerLimit::Tokens,
            required: 3,
            limit: 2,
        })
    );

    let diagnostic_source = source(b"@@".to_vec());
    assert_eq!(
        lex(
            &diagnostic_source,
            SemanticProfile::Blu,
            LexerLimits {
                max_diagnostics: 1,
                ..LexerLimits::default()
            },
        ),
        Err(LexError::Limit {
            kind: LexerLimit::Diagnostics,
            required: 2,
            limit: 1,
        })
    );
}

#[test]
fn diagnostic_value_limits_map_through_lex_error() {
    let source = source(b"@".to_vec());
    assert!(matches!(
        lex(
            &source,
            SemanticProfile::Blu,
            LexerLimits {
                diagnostic_limits: DiagnosticLimits {
                    max_found_bytes: 0,
                    ..DiagnosticLimits::default()
                },
                ..LexerLimits::default()
            },
        ),
        Err(LexError::Diagnostic(DiagnosticError::Limit {
            kind: DiagnosticLimit::FoundBytes,
            required: 1,
            limit: 0,
        }))
    ));
}

#[test]
fn only_a_byte_zero_directive_participates_in_reconciliation() {
    let source = source(b"\n--!dialect lua54\nreturn 1".to_vec());
    let lexed = lex(&source, SemanticProfile::Lua53, LexerLimits::default()).unwrap();

    assert_eq!(lexed.directive(), None);
    assert!(!lexed.has_errors());
    assert_eq!(
        lexed
            .tokens()
            .iter()
            .filter(|token| token.kind() == TokenKind::Comment)
            .count(),
        1
    );
}
