use blu_bytecode::blu::{
    BluLimits, Constant, FeatureBits, Instruction, MAGIC, TranslationError, decode_validated,
    translate_baseline_to_luau,
};
use blu_compiler::owned::{
    OwnedCompileError, OwnedCompileLimit, OwnedCompileLimits, OwnedCompiler,
};
use blu_core::{
    CompilerId, CompilerIdentity, DiagnosticError, DiagnosticLimit, IdentityLimits, Phase,
    SemanticProfile, Severity, SourceFile, SourceId, SourceLimits,
};
use blu_runtime::{Dialect, Value, Vm};
use sha2::{Digest, Sha256};

fn make_source(bytes: impl Into<Vec<u8>>) -> SourceFile {
    SourceFile::new(
        SourceId::new(23),
        "owned-slice.blu",
        bytes,
        SourceLimits::default(),
    )
    .unwrap()
}

fn compiler_identity() -> CompilerIdentity {
    CompilerIdentity::new(
        CompilerId::new([0x42; 16]),
        "blu-owned-test",
        "0.1.0",
        Some("751d78e".to_owned()),
        IdentityLimits::default(),
    )
    .unwrap()
}

fn span_of(source: &SourceFile, needle: &[u8]) -> blu_core::ByteSpan {
    let start = source
        .bytes()
        .windows(needle.len())
        .position(|candidate| candidate == needle)
        .expect("test source contains span");
    source.span(start, start + needle.len()).unwrap()
}

fn dialect(profile: SemanticProfile) -> Dialect {
    match profile {
        SemanticProfile::Blu => Dialect::Blu,
        SemanticProfile::Luau => Dialect::Luau,
        _ => panic!("test helper received unsupported profile"),
    }
}

#[test]
fn owned_vertical_slice_round_trips_and_executes_for_blu_and_luau() {
    for profile in [SemanticProfile::Blu, SemanticProfile::Luau] {
        let directive = match profile {
            SemanticProfile::Blu => b"--!dialect blu\n".as_slice(),
            SemanticProfile::Luau => b"--!dialect luau\n".as_slice(),
            _ => unreachable!(),
        };
        let mut bytes = directive.to_vec();
        bytes.extend_from_slice(b"local answer = 40\nreturn answer + 2");
        let source = make_source(bytes);
        let compiled = OwnedCompiler::default()
            .compile(&source, profile, compiler_identity())
            .unwrap();

        assert!(compiled.bytes().starts_with(&MAGIC));
        let artifact = compiled.artifact();
        assert_eq!(artifact.compiler().name(), "blu-owned-test");
        assert_eq!(artifact.compiler().revision(), Some("751d78e"));
        assert_eq!(artifact.sources()[0].identity.id(), SourceId::new(23));
        assert_eq!(artifact.sources()[0].identity.name(), "owned-slice.blu");
        assert_eq!(artifact.sources()[0].byte_len as usize, source.len());
        assert_eq!(
            artifact.sources()[0].digest,
            <[u8; 32]>::from(Sha256::digest(source.bytes()))
        );
        assert_eq!(artifact.main().profile, profile);
        assert_eq!(artifact.main().register_count, 3);
        assert_eq!(
            artifact.main().constants,
            [Constant::Number(40.0), Constant::Number(2.0)]
        );
        assert_eq!(
            artifact.main().code,
            [
                Instruction::LoadConstant {
                    destination: 0,
                    constant: 0
                },
                Instruction::LoadConstant {
                    destination: 1,
                    constant: 1
                },
                Instruction::Add {
                    destination: 2,
                    left: 0,
                    right: 1
                },
                Instruction::Return { first: 2, count: 1 },
            ]
        );
        assert_eq!(
            artifact.main().source_map,
            [
                span_of(&source, b"40"),
                span_of(&source, b"2"),
                span_of(&source, b"answer + 2"),
                span_of(&source, b"return answer + 2"),
            ]
        );
        assert_eq!(artifact.main().locals.len(), 1);
        assert_eq!(artifact.main().locals[0].name, b"answer");
        assert_eq!(artifact.main().locals[0].register, 0);
        assert_eq!(artifact.main().locals[0].start_pc, 1);
        assert_eq!(artifact.main().locals[0].end_pc, 4);

        let decoded = decode_validated(compiled.bytes(), BluLimits::default()).unwrap();
        assert_eq!(&decoded, compiled.artifact());

        let translated = translate_baseline_to_luau(
            compiled.into_validated_artifact(),
            profile,
            BluLimits::default(),
        )
        .unwrap();
        assert_eq!(
            Vm::new(dialect(profile)).execute_translated(translated),
            Ok(vec![Value::Number(42.0)])
        );
    }
}

#[test]
fn final_return_expression_list_is_emitted_contiguously() {
    let source = make_source(b"return 20, 22".to_vec());
    let compiled = OwnedCompiler::default()
        .compile(&source, SemanticProfile::Blu, compiler_identity())
        .unwrap();
    assert_eq!(
        compiled.artifact().main().code.last(),
        Some(&Instruction::Return { first: 0, count: 2 })
    );
    let translated = translate_baseline_to_luau(
        compiled.into_validated_artifact(),
        SemanticProfile::Blu,
        BluLimits::default(),
    )
    .unwrap();
    assert_eq!(
        Vm::new(Dialect::Blu).execute_translated(translated),
        Ok(vec![Value::Number(20.0), Value::Number(22.0)])
    );
}

#[test]
fn local_resolution_is_sequential_and_shadow_aware() {
    let source = make_source(b"local base = 40\nlocal answer = base\nreturn answer + 2".to_vec());
    let compiled = OwnedCompiler::default()
        .compile(&source, SemanticProfile::Luau, compiler_identity())
        .unwrap();
    assert_eq!(compiled.artifact().main().locals.len(), 2);
    assert_eq!(compiled.artifact().main().locals[0].register, 0);
    assert_eq!(compiled.artifact().main().locals[1].register, 0);

    let shadowed = make_source(b"local answer = 1\nlocal answer = 40\nreturn answer + 2".to_vec());
    let compiled = OwnedCompiler::default()
        .compile(&shadowed, SemanticProfile::Luau, compiler_identity())
        .unwrap();
    let translated = translate_baseline_to_luau(
        compiled.into_validated_artifact(),
        SemanticProfile::Luau,
        BluLimits::default(),
    )
    .unwrap();
    assert_eq!(
        Vm::new(Dialect::Luau).execute_translated(translated),
        Ok(vec![Value::Number(42.0)])
    );

    let unresolved = make_source(b"return missing + 2".to_vec());
    let error = OwnedCompiler::default()
        .compile(&unresolved, SemanticProfile::Blu, compiler_identity())
        .unwrap_err();
    let diagnostic = error.diagnostic().expect("resolve diagnostic");
    assert_eq!(diagnostic.code().as_str(), "BLU-RESOLVE-0001");
    assert_eq!(diagnostic.phase(), Phase::Resolve);
    assert_eq!(diagnostic.profile(), SemanticProfile::Blu);
    assert_eq!(diagnostic.severity(), Severity::Error);
    assert_eq!(
        unresolved.slice(diagnostic.primary().span()).unwrap(),
        b"missing"
    );
}

#[test]
fn floor_division_lowers_only_for_syntax_profiles_and_stays_non_executable() {
    for profile in [
        SemanticProfile::Luau,
        SemanticProfile::Lua53,
        SemanticProfile::Lua54,
        SemanticProfile::Lua55,
    ] {
        let source = make_source(b"return 40 // 2".to_vec());
        let compiled = OwnedCompiler::default()
            .compile(&source, profile, compiler_identity())
            .unwrap();
        assert!(
            compiled
                .artifact()
                .main()
                .required_features
                .contains(FeatureBits::FLOOR_DIVISION)
        );
        assert_eq!(
            compiled.artifact().main().code,
            [
                Instruction::LoadConstant {
                    destination: 0,
                    constant: 0,
                },
                Instruction::LoadConstant {
                    destination: 1,
                    constant: 1,
                },
                Instruction::FloorDivide {
                    destination: 2,
                    left: 0,
                    right: 1,
                },
                Instruction::Return { first: 2, count: 1 },
            ]
        );
        assert_eq!(
            compiled.artifact().main().source_map[2],
            source.span(7, source.len()).unwrap()
        );

        if profile == SemanticProfile::Luau {
            assert_eq!(
                translate_baseline_to_luau(
                    compiled.into_validated_artifact(),
                    profile,
                    BluLimits::default(),
                ),
                Err(TranslationError::UnsupportedInstruction {
                    prototype: 0,
                    instruction: "floor division",
                })
            );
        }
    }

    let source = make_source(b"return 40 // 2".to_vec());
    let error = OwnedCompiler::default()
        .compile(&source, SemanticProfile::Blu, compiler_identity())
        .unwrap_err();
    let diagnostic = error.diagnostic().expect("lowering diagnostic");
    assert_eq!(diagnostic.code().as_str(), "BLU-LOWER-0001");
    assert_eq!(diagnostic.phase(), Phase::Lower);
    assert_eq!(diagnostic.profile(), SemanticProfile::Blu);
    assert_eq!(source.slice(diagnostic.primary().span()).unwrap(), b"//");

    for profile in [SemanticProfile::Lua51, SemanticProfile::Lua52] {
        let source = make_source(b"return 40 // 2".to_vec());
        let error = OwnedCompiler::default()
            .compile(&source, profile, compiler_identity())
            .unwrap_err();
        let rejected = error.syntax().expect("lexical profile rejection");
        let diagnostic = &rejected.diagnostics()[0];
        assert_eq!(diagnostic.code().as_str(), "BLU-LEX-0002");
        assert_eq!(diagnostic.phase(), Phase::Lex);
        assert_eq!(diagnostic.profile(), profile);
        assert_eq!(source.slice(diagnostic.primary().span()).unwrap(), b"//");
    }
}

#[test]
fn syntax_profiles_and_missing_return_fail_structurally() {
    let malformed = make_source(b"local = 40".to_vec());
    let error = OwnedCompiler::default()
        .compile(&malformed, SemanticProfile::Blu, compiler_identity())
        .unwrap_err();
    let rejected = error.syntax().expect("syntax rejection");
    assert_eq!(rejected.diagnostics()[0].code().as_str(), "BLU-PARSE-0002");

    let no_return = make_source(b"local answer = 40".to_vec());
    let error = OwnedCompiler::default()
        .compile(&no_return, SemanticProfile::Blu, compiler_identity())
        .unwrap_err();
    let diagnostic = error.diagnostic().expect("missing-return diagnostic");
    assert_eq!(diagnostic.code().as_str(), "BLU-LOWER-0003");
    assert_eq!(diagnostic.phase(), Phase::Lower);
    assert_eq!(diagnostic.profile(), SemanticProfile::Blu);
    assert_eq!(diagnostic.severity(), Severity::Error);
    assert_eq!(
        diagnostic.primary().span(),
        no_return.span(0, no_return.len()).unwrap()
    );

    let unresolved = make_source(b"return missing".to_vec());
    let mut limits = OwnedCompileLimits::default();
    limits.parse.lexer.diagnostic_limits.max_label_message_bytes = 1;
    assert!(matches!(
        OwnedCompiler::new(limits).compile(&unresolved, SemanticProfile::Blu, compiler_identity(),),
        Err(OwnedCompileError::DiagnosticConstruction(
            DiagnosticError::Limit {
                kind: DiagnosticLimit::LabelMessageBytes,
                limit: 1,
                ..
            }
        ))
    ));
}

#[test]
fn compiler_limits_fail_before_artifact_creation() {
    let source = make_source(b"return 40 + 2".to_vec());
    let compiler = OwnedCompiler::new(OwnedCompileLimits {
        max_constants: 1,
        ..OwnedCompileLimits::default()
    });
    assert!(matches!(
        compiler.compile(&source, SemanticProfile::Blu, compiler_identity()),
        Err(OwnedCompileError::Limit {
            kind: OwnedCompileLimit::Constants,
            required: 2,
            limit: 1,
        })
    ));

    let compiler = OwnedCompiler::new(OwnedCompileLimits {
        max_registers: 2,
        ..OwnedCompileLimits::default()
    });
    assert!(matches!(
        compiler.compile(&source, SemanticProfile::Blu, compiler_identity()),
        Err(OwnedCompileError::Limit {
            kind: OwnedCompileLimit::Registers,
            required: 3,
            limit: 2,
        })
    ));
}

#[test]
fn shared_baseline_artifacts_round_trip_for_all_seven_profiles() {
    for profile in SemanticProfile::ALL {
        let source = make_source(b"local answer = 40\nreturn answer + 2".to_vec());
        let compiled = OwnedCompiler::default()
            .compile(&source, profile, compiler_identity())
            .unwrap();
        assert_eq!(compiled.artifact().main().profile, profile);
        let integer_profile = matches!(
            profile,
            SemanticProfile::Lua53 | SemanticProfile::Lua54 | SemanticProfile::Lua55
        );
        if integer_profile {
            assert_eq!(
                compiled.artifact().main().constants,
                [Constant::Integer(40), Constant::Integer(2)]
            );
            assert_eq!(
                compiled.artifact().main().required_features,
                FeatureBits::BASELINE | FeatureBits::INTEGER_CONSTANTS
            );
        } else {
            assert_eq!(
                compiled.artifact().main().constants,
                [Constant::Number(40.0), Constant::Number(2.0)]
            );
            assert_eq!(
                compiled.artifact().main().required_features,
                FeatureBits::BASELINE
            );
        }
        let decoded = decode_validated(compiled.bytes(), BluLimits::default()).unwrap();
        assert_eq!(decoded.main().profile, profile);
        assert_eq!(&decoded, compiled.artifact());
    }
}

#[test]
fn decimal_constants_follow_each_profile_numeric_policy() {
    let number_source = make_source(b"return 9007199254740993, 18446744073709551616".to_vec());
    for profile in [
        SemanticProfile::Lua51,
        SemanticProfile::Lua52,
        SemanticProfile::Luau,
        SemanticProfile::Blu,
    ] {
        let compiled = OwnedCompiler::default()
            .compile(&number_source, profile, compiler_identity())
            .unwrap();
        assert_eq!(
            compiled.artifact().main().constants,
            [
                Constant::Number("9007199254740993".parse::<f64>().unwrap()),
                Constant::Number("18446744073709551616".parse::<f64>().unwrap()),
            ]
        );
        assert_eq!(
            compiled.artifact().main().required_features,
            FeatureBits::BASELINE
        );
    }

    let integer_then_float =
        make_source(b"return 9223372036854775807, 9223372036854775808".to_vec());
    for profile in [
        SemanticProfile::Lua53,
        SemanticProfile::Lua54,
        SemanticProfile::Lua55,
    ] {
        let compiled = OwnedCompiler::default()
            .compile(&integer_then_float, profile, compiler_identity())
            .unwrap();
        assert_eq!(
            compiled.artifact().main().constants,
            [
                Constant::Integer(i64::MAX),
                Constant::Number("9223372036854775808".parse::<f64>().unwrap()),
            ]
        );
        assert_eq!(
            compiled.artifact().main().required_features,
            FeatureBits::BASELINE | FeatureBits::INTEGER_CONSTANTS
        );
    }

    let twenty_one_digits = make_source(b"return 184467440737095516160".to_vec());
    let compiled = OwnedCompiler::default()
        .compile(
            &twenty_one_digits,
            SemanticProfile::Lua54,
            compiler_identity(),
        )
        .unwrap();
    assert_eq!(
        compiled.artifact().main().constants,
        [Constant::Number(
            "184467440737095516160".parse::<f64>().unwrap()
        )]
    );
    assert_eq!(
        compiled.artifact().main().required_features,
        FeatureBits::BASELINE
    );

    let compiler = OwnedCompiler::new(OwnedCompileLimits {
        max_integer_literal_bytes: 20,
        ..OwnedCompileLimits::default()
    });
    assert!(matches!(
        compiler.compile(
            &twenty_one_digits,
            SemanticProfile::Lua54,
            compiler_identity(),
        ),
        Err(OwnedCompileError::Limit {
            kind: OwnedCompileLimit::IntegerLiteralBytes,
            required: 21,
            limit: 20,
        })
    ));
}

#[test]
fn source_and_debug_name_limits_are_checked_before_owned_copies() {
    let source = make_source(b"local answer = 1\nreturn answer".to_vec());
    let mut source_name_limits = OwnedCompileLimits::default();
    source_name_limits.artifact.identity.max_source_name_bytes = 3;
    assert!(matches!(
        OwnedCompiler::new(source_name_limits).compile(
            &source,
            SemanticProfile::Blu,
            compiler_identity(),
        ),
        Err(OwnedCompileError::Limit {
            kind: OwnedCompileLimit::SourceNameBytes,
            required: 15,
            limit: 3,
        })
    ));

    let mut debug_name_limits = OwnedCompileLimits::default();
    debug_name_limits.artifact.max_debug_name_bytes = 3;
    assert!(matches!(
        OwnedCompiler::new(debug_name_limits).compile(
            &source,
            SemanticProfile::Blu,
            compiler_identity(),
        ),
        Err(OwnedCompileError::Limit {
            kind: OwnedCompileLimit::DebugNameBytes,
            required: 6,
            limit: 3,
        })
    ));

    let source = make_source(b"local a = 1\nlocal b = 2\nreturn a + b".to_vec());
    let mut total_debug_limits = OwnedCompileLimits::default();
    total_debug_limits.artifact.max_total_debug_bytes = 1;
    assert!(matches!(
        OwnedCompiler::new(total_debug_limits).compile(
            &source,
            SemanticProfile::Blu,
            compiler_identity(),
        ),
        Err(OwnedCompileError::Limit {
            kind: OwnedCompileLimit::TotalDebugBytes,
            required: 2,
            limit: 1,
        })
    ));
}

#[test]
fn artifact_register_limit_is_separate_from_bootstrap_translation_limit() {
    use core::fmt::Write;

    let mut text = String::new();
    for index in 0..256 {
        writeln!(&mut text, "local value{index} = 1").unwrap();
    }
    text.push_str("return value255");
    let source = make_source(text.into_bytes());
    let compiled = OwnedCompiler::default()
        .compile(&source, SemanticProfile::Blu, compiler_identity())
        .unwrap();
    assert_eq!(compiled.artifact().main().register_count, 256);
    assert!(matches!(
        translate_baseline_to_luau(
            compiled.into_validated_artifact(),
            SemanticProfile::Blu,
            BluLimits::default(),
        ),
        Err(TranslationError::TooLarge {
            prototype: Some(0),
            what: "register count",
            actual: 256,
            limit: 255,
        })
    ));
}

#[test]
fn noncontiguous_return_values_are_normalized_with_numeric_baseline_ops() {
    let source = make_source(b"local a = 1\nlocal b = 2\nreturn b, a".to_vec());
    let compiled = OwnedCompiler::default()
        .compile(&source, SemanticProfile::Blu, compiler_identity())
        .unwrap();
    let return_start = source
        .bytes()
        .windows(b"return b, a".len())
        .position(|candidate| candidate == b"return b, a")
        .unwrap();
    let returned_b = source.span(return_start + 7, return_start + 8).unwrap();
    let returned_a = source.span(return_start + 10, return_start + 11).unwrap();
    assert_eq!(
        compiled.artifact().main().source_map,
        [
            span_of(&source, b"1"),
            span_of(&source, b"2"),
            returned_b,
            returned_b,
            returned_a,
            source
                .span(return_start, return_start + b"return b, a".len())
                .unwrap(),
        ]
    );
    assert!(matches!(
        compiled.artifact().main().code.last(),
        Some(Instruction::Return { count: 2, .. })
    ));
    let translated = translate_baseline_to_luau(
        compiled.into_validated_artifact(),
        SemanticProfile::Blu,
        BluLimits::default(),
    )
    .unwrap();
    assert_eq!(
        Vm::new(Dialect::Blu).execute_translated(translated),
        Ok(vec![Value::Number(2.0), Value::Number(1.0)])
    );
}
