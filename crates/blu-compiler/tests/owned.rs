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
fn uninitialized_local_lowers_to_nil_for_every_profile() {
    for profile in SemanticProfile::ALL {
        let source = make_source(b"local missing\nreturn missing".to_vec());
        let compiled = OwnedCompiler::default()
            .compile(&source, profile, compiler_identity())
            .unwrap();
        assert_eq!(compiled.artifact().main().constants, [Constant::Nil]);
        assert_eq!(
            compiled.artifact().main().code,
            [
                Instruction::LoadConstant {
                    destination: 0,
                    constant: 0,
                },
                Instruction::Return { first: 0, count: 1 },
            ]
        );
        assert_eq!(
            Vm::new(Dialect::Blu)
                .execute_blu_v1(compiled.into_validated_artifact(), BluLimits::default()),
            Ok(vec![Value::Nil]),
            "{profile}"
        );
    }
}

#[test]
fn local_lists_evaluate_before_binding_and_fill_missing_values_with_nil() {
    for profile in SemanticProfile::ALL {
        let source = make_source(
            b"local value = 40\nlocal value, next, missing = value, value + 2\nreturn value, next, missing"
                .to_vec(),
        );
        let compiled = OwnedCompiler::default()
            .compile(&source, profile, compiler_identity())
            .unwrap();
        let numeric = matches!(
            profile,
            SemanticProfile::Lua53 | SemanticProfile::Lua54 | SemanticProfile::Lua55
        );
        assert_eq!(
            Vm::new(Dialect::Blu)
                .execute_blu_v1(compiled.into_validated_artifact(), BluLimits::default()),
            Ok(vec![
                if numeric {
                    Value::Integer(40)
                } else {
                    Value::Number(40.0)
                },
                if numeric {
                    Value::Integer(42)
                } else {
                    Value::Number(42.0)
                },
                Value::Nil,
            ]),
            "{profile}"
        );
    }

    let source = make_source(b"local kept = 1, 2\nreturn kept".to_vec());
    let compiled = OwnedCompiler::default()
        .compile(&source, SemanticProfile::Blu, compiler_identity())
        .unwrap();
    assert_eq!(
        compiled.artifact().main().constants,
        [Constant::Number(1.0), Constant::Number(2.0)]
    );
    assert_eq!(
        Vm::new(Dialect::Blu)
            .execute_blu_v1(compiled.into_validated_artifact(), BluLimits::default()),
        Ok(vec![Value::Number(1.0)])
    );

    let source = make_source(b"local value\nvalue, value = 1, 2\nreturn value".to_vec());
    let compiled = OwnedCompiler::default()
        .compile(&source, SemanticProfile::Blu, compiler_identity())
        .unwrap();
    assert_eq!(
        Vm::new(Dialect::Blu)
            .execute_blu_v1(compiled.into_validated_artifact(), BluLimits::default()),
        Ok(vec![Value::Number(2.0)])
    );
}

#[test]
fn local_assignment_mutates_the_active_shadowed_binding_for_every_profile() {
    for profile in SemanticProfile::ALL {
        let source = make_source(
            b"local answer = 1\nlocal answer = 40\nanswer = answer + 2\nreturn answer".to_vec(),
        );
        let compiled = OwnedCompiler::default()
            .compile(&source, profile, compiler_identity())
            .unwrap();
        assert!(compiled.artifact().main().code.iter().any(|instruction| {
            matches!(
                instruction,
                Instruction::Move {
                    destination: 1,
                    source: 3,
                }
            )
        }));
        assert_eq!(
            Vm::new(Dialect::Blu)
                .execute_blu_v1(compiled.into_validated_artifact(), BluLimits::default()),
            Ok(vec![if matches!(
                profile,
                SemanticProfile::Lua53 | SemanticProfile::Lua54 | SemanticProfile::Lua55
            ) {
                Value::Integer(42)
            } else {
                Value::Number(42.0)
            }]),
            "{profile}"
        );
    }

    let source = make_source(b"missing = 1".to_vec());
    let error = OwnedCompiler::default()
        .compile(&source, SemanticProfile::Blu, compiler_identity())
        .unwrap_err();
    let diagnostic = error.diagnostic().unwrap();
    assert_eq!(diagnostic.code().as_str(), "BLU-RESOLVE-0001");
    assert_eq!(diagnostic.phase(), Phase::Resolve);
    assert_eq!(
        source.slice(diagnostic.primary().span()).unwrap(),
        b"missing"
    );
}

#[test]
fn assignment_lists_snapshot_rhs_and_adjust_fixed_scalar_values() {
    for profile in SemanticProfile::ALL {
        let source = make_source(
            b"local first, second = 1, 2\nfirst, second = second, first\nreturn first, second"
                .to_vec(),
        );
        let compiled = OwnedCompiler::default()
            .compile(&source, profile, compiler_identity())
            .unwrap();
        let numeric = matches!(
            profile,
            SemanticProfile::Lua53 | SemanticProfile::Lua54 | SemanticProfile::Lua55
        );
        assert_eq!(
            Vm::new(Dialect::Blu)
                .execute_blu_v1(compiled.into_validated_artifact(), BluLimits::default()),
            Ok(vec![
                if numeric {
                    Value::Integer(2)
                } else {
                    Value::Number(2.0)
                },
                if numeric {
                    Value::Integer(1)
                } else {
                    Value::Number(1.0)
                },
            ]),
            "{profile}"
        );
    }

    let source =
        make_source(b"local first, missing\nfirst, missing = 9\nreturn first, missing".to_vec());
    let compiled = OwnedCompiler::default()
        .compile(&source, SemanticProfile::Blu, compiler_identity())
        .unwrap();
    assert_eq!(
        Vm::new(Dialect::Blu)
            .execute_blu_v1(compiled.into_validated_artifact(), BluLimits::default()),
        Ok(vec![Value::Number(9.0), Value::Nil])
    );

    let source = make_source(b"local kept\nkept = 1, 2\nreturn kept".to_vec());
    let compiled = OwnedCompiler::default()
        .compile(&source, SemanticProfile::Blu, compiler_identity())
        .unwrap();
    assert_eq!(
        compiled.artifact().main().constants,
        [Constant::Nil, Constant::Number(1.0), Constant::Number(2.0)]
    );
    assert_eq!(
        Vm::new(Dialect::Blu)
            .execute_blu_v1(compiled.into_validated_artifact(), BluLimits::default()),
        Ok(vec![Value::Number(1.0)])
    );
}

#[test]
fn floor_division_lowers_only_for_assigned_profiles_and_bootstrap_translation_rejects_it() {
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
fn syntax_failures_are_structured_and_implicit_returns_execute() {
    let malformed = make_source(b"local = 40".to_vec());
    let error = OwnedCompiler::default()
        .compile(&malformed, SemanticProfile::Blu, compiler_identity())
        .unwrap_err();
    let rejected = error.syntax().expect("syntax rejection");
    assert_eq!(rejected.diagnostics()[0].code().as_str(), "BLU-PARSE-0002");

    for bytes in [b"".as_slice(), b"local answer = 40".as_slice()] {
        let source = make_source(bytes.to_vec());
        for profile in SemanticProfile::ALL {
            let compiled = OwnedCompiler::default()
                .compile(&source, profile, compiler_identity())
                .unwrap();
            assert_eq!(
                compiled.artifact().main().code.last(),
                Some(&Instruction::Return { first: 0, count: 0 }),
                "{profile}"
            );
            assert_eq!(
                compiled.artifact().main().source_map.last(),
                Some(&source.span(source.len(), source.len()).unwrap()),
                "{profile}"
            );
            assert_eq!(
                Vm::new(Dialect::Blu)
                    .execute_blu_v1(compiled.into_validated_artifact(), BluLimits::default(),),
                Ok(Vec::new()),
                "{profile}"
            );
        }
    }

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

    let empty = make_source(Vec::new());
    let compiler = OwnedCompiler::new(OwnedCompileLimits {
        max_instructions: 0,
        ..OwnedCompileLimits::default()
    });
    assert!(matches!(
        compiler.compile(&empty, SemanticProfile::Blu, compiler_identity()),
        Err(OwnedCompileError::Limit {
            kind: OwnedCompileLimit::Instructions,
            required: 1,
            limit: 0,
        })
    ));

    let local_list = make_source(b"local first, second".to_vec());
    let compiler = OwnedCompiler::new(OwnedCompileLimits {
        max_bindings: 1,
        ..OwnedCompileLimits::default()
    });
    assert!(matches!(
        compiler.compile(&local_list, SemanticProfile::Blu, compiler_identity()),
        Err(OwnedCompileError::Limit {
            kind: OwnedCompileLimit::Bindings,
            required: 2,
            limit: 1,
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
fn ordinary_division_lowers_for_every_profile() {
    let source = make_source(b"return 21 / 2, 20 / 5".to_vec());
    for profile in SemanticProfile::ALL {
        let compiled = OwnedCompiler::default()
            .compile(&source, profile, compiler_identity())
            .unwrap();
        assert!(
            compiled
                .artifact()
                .main()
                .code
                .iter()
                .any(|instruction| matches!(instruction, Instruction::Divide { .. })),
            "{profile}"
        );
    }
}

#[test]
fn modulo_lowers_for_every_profile() {
    let source = make_source(b"return -7 % 3, 7 % -3".to_vec());
    for profile in SemanticProfile::ALL {
        let compiled = OwnedCompiler::default()
            .compile(&source, profile, compiler_identity())
            .unwrap();
        assert_eq!(
            compiled
                .artifact()
                .main()
                .code
                .iter()
                .filter(|instruction| matches!(instruction, Instruction::Modulo { .. }))
                .count(),
            2,
            "{profile}"
        );
    }
}

#[test]
fn exponentiation_lowers_for_every_profile() {
    let source = make_source(b"return -2^2, 2^-2, 2^3^2".to_vec());
    for profile in SemanticProfile::ALL {
        let compiled = OwnedCompiler::default()
            .compile(&source, profile, compiler_identity())
            .unwrap();
        assert_eq!(
            compiled
                .artifact()
                .main()
                .code
                .iter()
                .filter(|instruction| matches!(instruction, Instruction::Power { .. }))
                .count(),
            4,
            "{profile}"
        );
    }
}

#[test]
fn unary_negation_lowers_for_every_profile() {
    let source = make_source(b"return -7, -(2 + 3), - -1".to_vec());
    for profile in SemanticProfile::ALL {
        let compiled = OwnedCompiler::default()
            .compile(&source, profile, compiler_identity())
            .unwrap();
        assert_eq!(
            compiled
                .artifact()
                .main()
                .code
                .iter()
                .filter(|instruction| matches!(instruction, Instruction::Negate { .. }))
                .count(),
            4,
            "{profile}"
        );
    }
}

#[test]
fn byte_string_length_lowers_for_every_profile() {
    let source = make_source(br#"return #'blu', #"a\nb", #''"#.to_vec());
    for profile in SemanticProfile::ALL {
        let compiled = OwnedCompiler::default()
            .compile(&source, profile, compiler_identity())
            .unwrap();
        assert_eq!(
            compiled
                .artifact()
                .main()
                .code
                .iter()
                .filter(|instruction| matches!(instruction, Instruction::Length { .. }))
                .count(),
            3,
            "{profile}"
        );
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
fn fractional_and_exponent_numbers_lower_for_every_profile() {
    let source = make_source(b"return 1.5, .25, 1., 1.e2, 2e3, 4.5E-2".to_vec());
    for profile in SemanticProfile::ALL {
        let compiled = OwnedCompiler::default()
            .compile(&source, profile, compiler_identity())
            .unwrap();
        assert_eq!(
            compiled.artifact().main().constants,
            [
                Constant::Number(1.5),
                Constant::Number(0.25),
                Constant::Number(1.0),
                Constant::Number(100.0),
                Constant::Number(2_000.0),
                Constant::Number(0.045),
            ],
            "{profile}"
        );
    }

    let limits = OwnedCompileLimits {
        max_number_literal_bytes: 4,
        ..OwnedCompileLimits::default()
    };
    assert!(matches!(
        OwnedCompiler::new(limits).compile(
            &make_source(b"return 12.34".to_vec()),
            SemanticProfile::Blu,
            compiler_identity(),
        ),
        Err(OwnedCompileError::Limit {
            kind: OwnedCompileLimit::NumberLiteralBytes,
            required: 5,
            limit: 4,
        })
    ));
}

#[test]
fn hexadecimal_integers_follow_each_profile_numeric_policy() {
    let source =
        make_source(b"return 0x10, 0Xff, 0xffffffffffffffff, 0x10000000000000000".to_vec());
    for profile in SemanticProfile::ALL {
        let compiled = OwnedCompiler::default()
            .compile(&source, profile, compiler_identity())
            .unwrap();
        let constants = compiled.artifact().prototypes()[0].constants.as_slice();
        if matches!(
            profile,
            SemanticProfile::Lua53 | SemanticProfile::Lua54 | SemanticProfile::Lua55
        ) {
            assert_eq!(
                constants,
                [
                    Constant::Integer(16),
                    Constant::Integer(255),
                    Constant::Integer(-1),
                    Constant::Integer(0),
                ],
                "{profile}"
            );
        } else {
            assert_eq!(
                constants,
                [
                    Constant::Number(16.0),
                    Constant::Number(255.0),
                    Constant::Number(18_446_744_073_709_551_615.0),
                    Constant::Number(18_446_744_073_709_551_616.0),
                ],
                "{profile}"
            );
        }
    }
}

#[test]
fn numeric_separators_lower_only_for_blu_and_luau() {
    let source = make_source(b"return 1_000, 12_345.1_25, 0xff_ff".to_vec());
    for profile in SemanticProfile::ALL {
        let result = OwnedCompiler::default().compile(&source, profile, compiler_identity());
        if matches!(profile, SemanticProfile::Blu | SemanticProfile::Luau) {
            let compiled = result.unwrap();
            assert_eq!(
                compiled.artifact().prototypes()[0].constants.as_slice(),
                [
                    Constant::Number(1_000.0),
                    Constant::Number(12_345.125),
                    Constant::Number(65_535.0),
                ],
                "{profile}"
            );
        } else {
            let error = result.unwrap_err();
            let rejected = error.syntax().expect("numeric separator rejection");
            assert_eq!(
                rejected.diagnostics()[0].code().as_str(),
                "BLU-LEX-0012",
                "{profile}"
            );
        }
    }
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
fn string_literal_payload_limits_fail_before_constant_insertion() {
    let source = make_source(b"return 'blu'".to_vec());
    let mut limits = OwnedCompileLimits::default();
    limits.artifact.max_constant_bytes = 2;
    assert!(matches!(
        OwnedCompiler::new(limits).compile(&source, SemanticProfile::Blu, compiler_identity(),),
        Err(OwnedCompileError::Limit {
            kind: OwnedCompileLimit::StringLiteralBytes,
            required: 3,
            limit: 2,
        })
    ));

    let source = make_source(b"return 'abc', 'de'".to_vec());
    let mut limits = OwnedCompileLimits::default();
    limits.artifact.max_total_constant_bytes = 4;
    assert!(matches!(
        OwnedCompiler::new(limits).compile(&source, SemanticProfile::Luau, compiler_identity(),),
        Err(OwnedCompileError::Limit {
            kind: OwnedCompileLimit::TotalConstantBytes,
            required: 5,
            limit: 4,
        })
    ));

    let source = make_source(br#"return "\n""#.to_vec());
    let mut limits = OwnedCompileLimits::default();
    limits.artifact.max_constant_bytes = 1;
    let compiled = OwnedCompiler::new(limits)
        .compile(&source, SemanticProfile::Lua54, compiler_identity())
        .unwrap();
    assert_eq!(
        compiled.artifact().main().constants,
        [Constant::String(vec![b'\n'])]
    );
}

#[test]
fn common_string_escapes_decode_to_bytes_for_every_profile() {
    let source = make_source(br#"return "\\\'\"\a\b\f\n\r\t\v""#.to_vec());
    let expected = b"\\'\"\x07\x08\x0c\n\r\t\x0b".to_vec();

    for profile in SemanticProfile::ALL {
        let compiled = OwnedCompiler::default()
            .compile(&source, profile, compiler_identity())
            .unwrap();
        assert_eq!(
            compiled.artifact().main().constants,
            [Constant::String(expected.clone())],
            "{profile}"
        );
    }
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
fn noncontiguous_return_values_are_normalized_with_moves() {
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
            returned_a,
            source
                .span(return_start, return_start + b"return b, a".len())
                .unwrap(),
        ]
    );
    assert!(matches!(
        &compiled.artifact().main().code[2..4],
        [
            Instruction::Move { source: 1, .. },
            Instruction::Move { source: 0, .. }
        ]
    ));
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
