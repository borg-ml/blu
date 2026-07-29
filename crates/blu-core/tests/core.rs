use blu_core::{
    ByteOffset, ByteSpan, CompilerId, CompilerIdentity, Diagnostic, DiagnosticCode,
    DiagnosticCodeError, IdentityError, IdentityLimits, Label, LineIndexError, Phase,
    SemanticProfile, Severity, SourceError, SourceFile, SourceId, SourceLimit, SourceLimits,
    SpanError,
};

#[test]
fn all_semantic_profiles_have_stable_names_and_round_trip() {
    let expected = ["blu", "luau", "lua51", "lua52", "lua53", "lua54", "lua55"];
    assert_eq!(SemanticProfile::ALL.map(SemanticProfile::as_str), expected);
    for (profile, name) in SemanticProfile::ALL.into_iter().zip(expected) {
        assert_eq!(name.parse(), Ok(profile));
        assert_eq!(profile.to_string(), name);
    }
    assert_eq!(
        "Lua54".parse::<SemanticProfile>().unwrap_err().value(),
        "Lua54"
    );
}

#[test]
fn source_and_compiler_identities_are_stable_and_bounded() {
    let limits = IdentityLimits::default();
    let compiler_id = CompilerId::new(*b"blu-core-test-id");
    let compiler = CompilerIdentity::new(
        compiler_id,
        "blu-compiler",
        "0.1.0",
        Some("f8ca77acdcb5".into()),
        limits,
    )
    .unwrap();
    assert_eq!(compiler.id().as_bytes(), b"blu-core-test-id");
    assert_eq!(compiler.name(), "blu-compiler");
    assert_eq!(compiler.version(), "0.1.0");
    assert_eq!(compiler.revision(), Some("f8ca77acdcb5"));

    assert!(matches!(
        CompilerIdentity::new(compiler_id, "", "0.1.0", None, IdentityLimits::default()),
        Err(IdentityError::Empty {
            what: "compiler name"
        })
    ));
    assert!(matches!(
        CompilerIdentity::new(
            compiler_id,
            "blu",
            "0.1.0",
            Some("bad\0revision".into()),
            IdentityLimits::default()
        ),
        Err(IdentityError::Nul {
            what: "compiler revision",
            ..
        })
    ));
}

#[test]
fn line_positions_are_zero_based_byte_offsets_for_lf_and_crlf() {
    let source = SourceFile::new(
        SourceId::new(7),
        "mixed-lines.lua",
        b"alpha\r\nbeta\nomega".to_vec(),
        SourceLimits::default(),
    )
    .unwrap();

    assert_eq!(source.line_index().line_count(), 3);
    assert_eq!(
        source.position(0).unwrap(),
        blu_core::LinePosition {
            line: 0,
            byte_column: 0
        }
    );
    assert_eq!(
        source.position(7).unwrap(),
        blu_core::LinePosition {
            line: 1,
            byte_column: 0
        }
    );
    assert_eq!(
        source.position(11).unwrap(),
        blu_core::LinePosition {
            line: 1,
            byte_column: 4
        }
    );
    assert_eq!(
        source.position(12).unwrap(),
        blu_core::LinePosition {
            line: 2,
            byte_column: 0
        }
    );

    let first = source.line_content_span(0).unwrap();
    let second = source.line_content_span(1).unwrap();
    let third = source.line_content_span(2).unwrap();
    assert_eq!(source.slice(first).unwrap(), b"alpha");
    assert_eq!(source.slice(second).unwrap(), b"beta");
    assert_eq!(source.slice(third).unwrap(), b"omega");
}

#[test]
fn source_contents_and_diagnostic_found_tokens_accept_non_utf8() {
    let source = SourceFile::new(
        SourceId::new(8),
        "binary.lua",
        vec![0xff, b'\n', 0xfe],
        SourceLimits::default(),
    )
    .unwrap();
    assert_eq!(source.bytes(), &[0xff, b'\n', 0xfe]);
    assert_eq!(source.line_index().line_count(), 2);
    assert_eq!(source.slice(source.span(2, 3).unwrap()).unwrap(), &[0xfe]);

    let diagnostic = Diagnostic::new(
        DiagnosticCode::new("BLU-LEX-0001").unwrap(),
        Phase::Lex,
        SemanticProfile::Lua54,
        Severity::Error,
        Label::new(source.span(0, 1).unwrap(), "invalid source byte"),
    )
    .with_found(vec![0xff]);
    assert_eq!(diagnostic.found(), Some(&[0xff][..]));
    assert_eq!(diagnostic.primary().span().start().get(), 0);
    assert_eq!(diagnostic.primary().span().end().get(), 1);
}

#[test]
fn invalid_and_overflowing_spans_fail_structurally() {
    let source_id = SourceId::new(1);
    assert!(matches!(
        ByteSpan::new(source_id, ByteOffset::new(9), ByteOffset::new(3)),
        Err(SpanError::Reversed { .. })
    ));
    assert!(matches!(
        ByteSpan::from_usize(source_id, 0, usize::MAX),
        Err(SpanError::OffsetOverflow { value: usize::MAX })
    ));

    let source = SourceFile::new(
        source_id,
        "small.lua",
        b"abc".to_vec(),
        SourceLimits::default(),
    )
    .unwrap();
    assert!(matches!(
        source.span(0, 4),
        Err(SpanError::OutOfBounds { .. })
    ));
    let foreign = ByteSpan::from_usize(SourceId::new(2), 0, 1).unwrap();
    assert!(matches!(
        source.slice(foreign),
        Err(SpanError::SourceMismatch { .. })
    ));
    assert!(matches!(
        source.position(4),
        Err(SourceError::LineIndex(
            LineIndexError::OffsetOutOfBounds { .. }
        ))
    ));
}

#[test]
fn source_byte_line_and_name_limits_fail_before_use() {
    let id = SourceId::new(3);
    assert!(matches!(
        SourceFile::new(
            id,
            "bytes.lua",
            b"1234".to_vec(),
            SourceLimits {
                max_bytes: 3,
                ..SourceLimits::default()
            }
        ),
        Err(SourceError::Limit {
            kind: SourceLimit::Bytes,
            actual: 4,
            limit: 3
        })
    ));
    assert!(matches!(
        SourceFile::new(
            id,
            "lines.lua",
            b"a\nb\nc".to_vec(),
            SourceLimits {
                max_lines: 2,
                ..SourceLimits::default()
            }
        ),
        Err(SourceError::Limit {
            kind: SourceLimit::Lines,
            actual: 3,
            limit: 2
        })
    ));
    assert!(matches!(
        SourceFile::new(
            id,
            "long-name.lua",
            Vec::new(),
            SourceLimits {
                max_name_bytes: 4,
                ..SourceLimits::default()
            }
        ),
        Err(SourceError::Limit {
            kind: SourceLimit::NameBytes,
            ..
        })
    ));
}

#[test]
fn diagnostics_are_structured_and_deterministic() {
    let source = SourceFile::new(
        SourceId::new(4),
        "diagnostic.lua",
        b"local =".to_vec(),
        SourceLimits::default(),
    )
    .unwrap();
    let primary = Label::new(source.span(6, 7).unwrap(), "expected a binding name");
    let earlier = Label::new(source.span(0, 5).unwrap(), "declaration starts here");
    let later = Label::new(source.span(5, 6).unwrap(), "whitespace before token");
    let code = DiagnosticCode::new("BLU-PARSE-0001").unwrap();

    let first = Diagnostic::new(
        code.clone(),
        Phase::Parse,
        SemanticProfile::Blu,
        Severity::Error,
        primary.clone(),
    )
    .with_secondary(later.clone())
    .with_secondary(earlier.clone())
    .with_expected("identifier")
    .with_expected("local function")
    .with_expected("identifier")
    .with_found(b"=".to_vec())
    .with_note("profile: blu")
    .with_help("name the local binding");

    let second = Diagnostic::new(
        code,
        Phase::Parse,
        SemanticProfile::Blu,
        Severity::Error,
        primary,
    )
    .with_secondary(earlier.clone())
    .with_secondary(later)
    .with_expected("local function")
    .with_expected("identifier")
    .with_found(b"=".to_vec())
    .with_note("profile: blu")
    .with_help("name the local binding");

    assert_eq!(first, second);
    assert_eq!(first.code().as_str(), "BLU-PARSE-0001");
    assert_eq!(first.phase(), Phase::Parse);
    assert_eq!(first.profile(), SemanticProfile::Blu);
    assert_eq!(first.severity(), Severity::Error);
    assert_eq!(
        first.secondary(),
        &[
            earlier,
            Label::new(source.span(5, 6).unwrap(), "whitespace before token")
        ]
    );
    assert_eq!(
        first.expected(),
        &["identifier".to_owned(), "local function".to_owned()]
    );
    assert_eq!(first.notes(), &["profile: blu".to_owned()]);
    assert_eq!(first.help(), &["name the local binding".to_owned()]);
}

#[test]
fn diagnostic_codes_reject_unstable_shapes() {
    assert_eq!(DiagnosticCode::new(""), Err(DiagnosticCodeError::Empty));
    assert!(matches!(
        DiagnosticCode::new("blu-parse-1"),
        Err(DiagnosticCodeError::InvalidStart { .. })
    ));
    assert!(matches!(
        DiagnosticCode::new("BLU_PARSE_1"),
        Err(DiagnosticCodeError::InvalidByte { .. })
    ));
}
