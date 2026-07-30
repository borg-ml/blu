use blu_bytecode::blu::{
    BluLimits, Constant, FeatureBits, Instruction, MAGIC, TranslationError, decode_validated,
    translate_baseline_to_luau,
};
use blu_compiler::owned::{
    OwnedCompileError, OwnedCompileLimit, OwnedCompileLimits, OwnedCompiler,
};
use blu_core::{
    CompilerId, CompilerIdentity, DiagnosticError, DiagnosticLimit, IdentityLimits, Phase,
    SemanticProfile, SourceFile, SourceId, SourceLimits,
};
use blu_runtime::{Dialect, RuntimeError, Value, Vm};
use sha2::{Digest, Sha256};
use std::sync::Arc;

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
    assert_ne!(
        compiled.artifact().main().locals[0].register,
        compiled.artifact().main().locals[1].register
    );

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
    let compiled = OwnedCompiler::default()
        .compile(&unresolved, SemanticProfile::Blu, compiler_identity())
        .expect("unresolved reads are globals");
    assert!(
        Vm::default()
            .execute_blu_v1(compiled.into_validated_artifact(), BluLimits::default())
            .is_err()
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
        let destination = compiled.artifact().main().locals[1].register;
        assert!(compiled.artifact().main().code.iter().any(
            |instruction| matches!(instruction, Instruction::Move { destination: register, .. } if *register == destination)
        ));
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
    let compiled = OwnedCompiler::default()
        .compile(&source, SemanticProfile::Blu, compiler_identity())
        .expect("unresolved assignment is global");
    let mut vm = Vm::default();
    assert_eq!(
        vm.execute_blu_v1(compiled.into_validated_artifact(), BluLimits::default()),
        Ok(Vec::new())
    );
    assert_eq!(vm.global(b"missing"), Some(&Value::Number(1.0)));
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

    for bytes in [
        b"first, second = 1, 2 first, second = second, first return first, second".as_slice(),
        b"local first, second = 1, 2 local function swap() first, second = second, first end swap() return first, second"
            .as_slice(),
    ] {
        let source = make_source(bytes.to_vec());
        let compiled = OwnedCompiler::default()
            .compile(&source, SemanticProfile::Blu, compiler_identity())
            .unwrap();
        assert_eq!(
            Vm::default().execute_blu_v1(
                compiled.into_validated_artifact(),
                BluLimits::default()
            ),
            Ok(vec![Value::Number(2.0), Value::Number(1.0)])
        );
    }
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

    let unresolved = make_source(b"local step = 1\nfor index = 1, 3, step do end".to_vec());
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
fn concatenation_is_canonical_profile_neutral_and_directly_executable() {
    let source = make_source(br#"return "a" .. 1 .. 2.5, 1 + 2 .. "x""#.to_vec());
    for profile in SemanticProfile::ALL {
        let compiled = OwnedCompiler::default()
            .compile(&source, profile, compiler_identity())
            .unwrap();
        assert!(
            compiled
                .artifact()
                .main()
                .required_features
                .contains(FeatureBits::CONCATENATION),
            "{profile}"
        );
        assert_eq!(
            compiled
                .artifact()
                .main()
                .code
                .iter()
                .filter(|instruction| matches!(instruction, Instruction::Concatenate { .. }))
                .count(),
            3,
            "{profile}"
        );
        if matches!(profile, SemanticProfile::Blu | SemanticProfile::Luau) {
            let translation_error = translate_baseline_to_luau(
                decode_validated(compiled.bytes(), BluLimits::default()).unwrap(),
                profile,
                BluLimits::default(),
            )
            .unwrap_err();
            assert!(
                matches!(
                    translation_error,
                    TranslationError::UnsupportedInstruction {
                        instruction: "concatenation",
                        ..
                    }
                ),
                "{profile}: {translation_error:?}"
            );
        }
        assert_eq!(
            Vm::default().execute_blu_v1(compiled.into_validated_artifact(), BluLimits::default()),
            Ok(vec![
                Value::String(std::sync::Arc::from(&b"a12.5"[..])),
                Value::String(std::sync::Arc::from(&b"3x"[..]))
            ]),
            "{profile}"
        );
    }
}

#[test]
fn owned_concatenation_invokes_resumable_metamethods() {
    let source = make_source(
        b"local left = setmetatable({}, {__concat = function(a, b) return a, b end}) local right = {} return left .. right"
            .to_vec(),
    );
    for profile in SemanticProfile::ALL {
        let compiled = OwnedCompiler::default()
            .compile(&source, profile, compiler_identity())
            .unwrap();
        let result = Vm::default()
            .execute_blu_v1(compiled.into_validated_artifact(), BluLimits::default())
            .unwrap();
        assert_eq!(result.len(), 1, "{profile}");
        assert!(matches!(result[0], Value::Table(_)), "{profile}");
    }
}

#[test]
fn owned_concatenation_uses_the_right_handler_when_the_left_has_none() {
    let source = make_source(
        b"local left = {} local right right = setmetatable({}, {__concat = function(a, b) return a == left and b == right end}) return left .. right"
            .to_vec(),
    );
    for profile in SemanticProfile::ALL {
        let compiled = OwnedCompiler::default()
            .compile(&source, profile, compiler_identity())
            .unwrap();
        assert_eq!(
            Vm::default().execute_blu_v1(compiled.into_validated_artifact(), BluLimits::default()),
            Ok(vec![Value::Boolean(true)]),
            "{profile}"
        );
    }
}

#[test]
fn comparisons_are_canonical_profile_neutral_and_directly_executable() {
    let source = make_source(
        br#"return 2 == 2, 2 ~= 3, 1 < 2, 2 <= 2, 3 > 2, 3 >= 3, "a" < "b", 1 == "1", 1 + 2 < 4, "ab" == "ab""#
            .to_vec(),
    );
    for profile in SemanticProfile::ALL {
        let compiled = OwnedCompiler::default()
            .compile(&source, profile, compiler_identity())
            .unwrap();
        assert!(
            compiled
                .artifact()
                .main()
                .required_features
                .contains(FeatureBits::COMPARISONS),
            "{profile}"
        );
        assert!(
            compiled
                .artifact()
                .main()
                .code
                .iter()
                .any(|instruction| matches!(
                    instruction,
                    Instruction::Equal { .. }
                        | Instruction::LessThan { .. }
                        | Instruction::LessEqual { .. }
                )),
            "{profile}"
        );
        if matches!(profile, SemanticProfile::Blu | SemanticProfile::Luau) {
            let translation_error = translate_baseline_to_luau(
                decode_validated(compiled.bytes(), BluLimits::default()).unwrap(),
                profile,
                BluLimits::default(),
            )
            .unwrap_err();
            assert!(
                matches!(
                    translation_error,
                    TranslationError::UnsupportedInstruction {
                        instruction: "comparisons",
                        ..
                    }
                ),
                "{profile}: {translation_error:?}"
            );
        }
        assert_eq!(
            Vm::default().execute_blu_v1(compiled.into_validated_artifact(), BluLimits::default()),
            Ok(vec![
                Value::Boolean(true),
                Value::Boolean(true),
                Value::Boolean(true),
                Value::Boolean(true),
                Value::Boolean(true),
                Value::Boolean(true),
                Value::Boolean(true),
                Value::Boolean(false),
                Value::Boolean(true),
                Value::Boolean(true),
            ]),
            "{profile}"
        );
    }
}

#[test]
fn owned_comparisons_invoke_resumable_profile_handlers() {
    let source = make_source(
        b"local mt = {__eq = function() return true end, __lt = function() return false end, __le = function() return true end} local a = setmetatable({}, mt) local b = setmetatable({}, mt) return a == b, a < b, a <= b"
            .to_vec(),
    );
    for profile in SemanticProfile::ALL {
        let compiled = OwnedCompiler::default()
            .compile(&source, profile, compiler_identity())
            .unwrap();
        assert_eq!(
            Vm::default().execute_blu_v1(compiled.into_validated_artifact(), BluLimits::default()),
            Ok(vec![
                Value::Boolean(true),
                Value::Boolean(false),
                Value::Boolean(true)
            ]),
            "{profile}"
        );
    }
}

#[test]
fn owned_equality_handler_selection_is_profile_specific() {
    let source = make_source(
        b"local a = setmetatable({}, {}) local b = setmetatable({}, {__eq = function() return true end}) return a == b"
            .to_vec(),
    );
    for profile in SemanticProfile::ALL {
        let compiled = OwnedCompiler::default()
            .compile(&source, profile, compiler_identity())
            .unwrap();
        let modern = matches!(
            profile,
            SemanticProfile::Blu
                | SemanticProfile::Lua53
                | SemanticProfile::Lua54
                | SemanticProfile::Lua55
        );
        assert_eq!(
            Vm::default().execute_blu_v1(compiled.into_validated_artifact(), BluLimits::default()),
            Ok(vec![Value::Boolean(modern)]),
            "{profile}"
        );
    }
}

#[test]
fn owned_less_equal_fallback_is_removed_only_in_lua55() {
    let source = make_source(
        b"local mt = {__lt = function() return false end} local a = setmetatable({}, mt) local b = setmetatable({}, mt) return a <= b"
            .to_vec(),
    );
    for profile in SemanticProfile::ALL {
        let compiled = OwnedCompiler::default()
            .compile(&source, profile, compiler_identity())
            .unwrap();
        let result =
            Vm::default().execute_blu_v1(compiled.into_validated_artifact(), BluLimits::default());
        if profile == SemanticProfile::Lua55 {
            assert!(
                matches!(result, Err(RuntimeError::Type { .. })),
                "{profile}"
            );
        } else {
            assert_eq!(result, Ok(vec![Value::Boolean(true)]), "{profile}");
        }
    }
}

#[test]
fn indexed_assignment_lists_snapshot_targets_and_rhs_before_committing() {
    let source = make_source(
        b"local left = {10} local right = {20} local index = 1 left[index], index, right[index] = right[index], 2, left[index] return left[1], right[1], index"
            .to_vec(),
    );
    let rebound = make_source(
        b"local value = {1} local original = value local other = {2} value[1], value = 9, other return original[1], value[1]"
            .to_vec(),
    );
    for profile in SemanticProfile::ALL {
        let modern = matches!(
            profile,
            SemanticProfile::Lua53 | SemanticProfile::Lua54 | SemanticProfile::Lua55
        );
        for (source, expected) in [
            (
                &source,
                if modern {
                    vec![Value::Integer(20), Value::Integer(10), Value::Integer(2)]
                } else {
                    vec![Value::Number(20.0), Value::Number(10.0), Value::Number(2.0)]
                },
            ),
            (
                &rebound,
                if modern {
                    vec![Value::Integer(9), Value::Integer(2)]
                } else {
                    vec![Value::Number(9.0), Value::Number(2.0)]
                },
            ),
        ] {
            let compiled = OwnedCompiler::default()
                .compile(source, profile, compiler_identity())
                .unwrap();
            assert_eq!(
                Vm::default()
                    .execute_blu_v1(compiled.into_validated_artifact(), BluLimits::default()),
                Ok(expected),
                "{profile}"
            );
        }
    }
}

#[test]
fn logical_operators_short_circuit_and_return_operands_for_every_profile() {
    let source = make_source(
        br#"return "left" and "right", nil or "fallback", false and (1 + "2"), true or (1 + "2"), false or nil, nil and "unused""#
            .to_vec(),
    );
    for profile in SemanticProfile::ALL {
        let compiled = OwnedCompiler::default()
            .compile(&source, profile, compiler_identity())
            .unwrap();
        assert!(
            compiled
                .artifact()
                .main()
                .required_features
                .contains(FeatureBits::FORWARD_BRANCHES),
            "{profile}"
        );
        assert!(
            compiled.artifact().main().code.iter().any(|instruction| {
                matches!(
                    instruction,
                    Instruction::JumpIfTruthy { .. } | Instruction::JumpIfFalsy { .. }
                )
            }),
            "{profile}"
        );
        if matches!(profile, SemanticProfile::Blu | SemanticProfile::Luau) {
            let translation_error = translate_baseline_to_luau(
                decode_validated(compiled.bytes(), BluLimits::default()).unwrap(),
                profile,
                BluLimits::default(),
            )
            .unwrap_err();
            assert!(
                matches!(
                    translation_error,
                    TranslationError::UnsupportedInstruction {
                        instruction: "forward branches",
                        ..
                    }
                ),
                "{profile}: {translation_error:?}"
            );
        }
        assert_eq!(
            Vm::default().execute_blu_v1(compiled.into_validated_artifact(), BluLimits::default()),
            Ok(vec![
                Value::String(std::sync::Arc::from(&b"right"[..])),
                Value::String(std::sync::Arc::from(&b"fallback"[..])),
                Value::Boolean(false),
                Value::Boolean(true),
                Value::Nil,
                Value::Nil,
            ]),
            "{profile}"
        );
    }
}

#[test]
fn conditional_blocks_execute_and_restore_local_scope_for_every_profile() {
    let source = make_source(
        br#"local value = "none"
if false then
    value = "bad"
elseif 1 < 2 then
    local selected = "selected"
    value = selected
else
    value = "else"
end
if true then
    value = value .. "!"
else
    value = "bad"
end
return value"#
            .to_vec(),
    );
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
                .any(|instruction| matches!(instruction, Instruction::Jump { .. })),
            "{profile}"
        );
        assert_eq!(
            Vm::default().execute_blu_v1(compiled.into_validated_artifact(), BluLimits::default()),
            Ok(vec![Value::String(std::sync::Arc::from(&b"selected!"[..]))]),
            "{profile}"
        );
    }

    let returned = make_source(b"if true then return \"then\" else return \"else\" end".to_vec());
    for profile in SemanticProfile::ALL {
        let compiled = OwnedCompiler::default()
            .compile(&returned, profile, compiler_identity())
            .unwrap();
        assert_eq!(
            Vm::default().execute_blu_v1(compiled.into_validated_artifact(), BluLimits::default()),
            Ok(vec![Value::String(std::sync::Arc::from(&b"then"[..]))]),
            "{profile}"
        );
    }

    let escaped = make_source(b"if true then local hidden = 1 end\nreturn hidden".to_vec());
    assert!(
        OwnedCompiler::default()
            .compile(&escaped, SemanticProfile::Blu, compiler_identity())
            .is_err()
    );
}

#[test]
fn while_loops_execute_with_scoped_locals_and_instruction_limits() {
    let source = make_source(
        b"local index = 0\nlocal total = 0\nwhile index < 5 do\nlocal next = index + 1\nindex = next\ntotal = total + index\nend\nreturn total, index"
            .to_vec(),
    );
    for profile in SemanticProfile::ALL {
        let compiled = OwnedCompiler::default()
            .compile(&source, profile, compiler_identity())
            .unwrap();
        assert!(
            compiled
                .artifact()
                .main()
                .required_features
                .contains(FeatureBits::BACKWARD_BRANCHES),
            "{profile}"
        );
        let expected = if matches!(
            profile,
            SemanticProfile::Lua53 | SemanticProfile::Lua54 | SemanticProfile::Lua55
        ) {
            vec![Value::Integer(15), Value::Integer(5)]
        } else {
            vec![Value::Number(15.0), Value::Number(5.0)]
        };
        assert_eq!(
            Vm::default().execute_blu_v1(compiled.into_validated_artifact(), BluLimits::default()),
            Ok(expected),
            "{profile}"
        );
    }

    let infinite = make_source(b"while true do end".to_vec());
    let compiled = OwnedCompiler::default()
        .compile(&infinite, SemanticProfile::Blu, compiler_identity())
        .unwrap();
    assert!(matches!(
        Vm::default()
            .with_instruction_limit(16)
            .execute_blu_v1(compiled.into_validated_artifact(), BluLimits::default()),
        Err(blu_runtime::RuntimeError::InstructionLimit { limit: 16 })
    ));
}

#[test]
fn break_exits_only_the_innermost_owned_loop() {
    let source = make_source(
        b"local outer = 0\nlocal hits = 0\nwhile outer < 3 do\nouter = outer + 1\nlocal inner = 0\nwhile true do\ninner = inner + 1\nhits = hits + 1\nif inner == 2 then break end\nend\nif outer == 2 then break end\nend\nreturn outer, hits"
            .to_vec(),
    );
    for profile in SemanticProfile::ALL {
        let compiled = OwnedCompiler::default()
            .compile(&source, profile, compiler_identity())
            .unwrap();
        let expected = if matches!(
            profile,
            SemanticProfile::Lua53 | SemanticProfile::Lua54 | SemanticProfile::Lua55
        ) {
            vec![Value::Integer(2), Value::Integer(4)]
        } else {
            vec![Value::Number(2.0), Value::Number(4.0)]
        };
        assert_eq!(
            Vm::default().execute_blu_v1(compiled.into_validated_artifact(), BluLimits::default()),
            Ok(expected),
            "{profile}"
        );
    }
}

#[test]
fn continue_restarts_only_blu_and_luau_owned_loops() {
    let source = make_source(
        b"local index = 0\nlocal total = 0\nwhile index < 5 do\nindex = index + 1\nif index % 2 == 0 then continue end\ntotal = total + index\nend\nreturn total"
            .to_vec(),
    );
    for profile in SemanticProfile::ALL {
        let compiled = OwnedCompiler::default().compile(&source, profile, compiler_identity());
        if matches!(profile, SemanticProfile::Blu | SemanticProfile::Luau) {
            assert_eq!(
                Vm::default().execute_blu_v1(
                    compiled.unwrap().into_validated_artifact(),
                    BluLimits::default()
                ),
                Ok(vec![Value::Number(9.0)]),
                "{profile}"
            );
        } else {
            let error = compiled.unwrap_err();
            assert!(
                matches!(error, OwnedCompileError::Syntax(_)),
                "{profile}: {error:?}"
            );
        }
    }
}

#[test]
fn repeat_until_executes_once_scopes_locals_and_tests_after_continue() {
    let source = make_source(
        b"local count = 0\nlocal total = 0\nrepeat\ncount = count + 1\nlocal current = count\nif count < 3 then continue end\ntotal = total + current\nuntil count == 4\nreturn count, total"
            .to_vec(),
    );
    for profile in [SemanticProfile::Blu, SemanticProfile::Luau] {
        let compiled = OwnedCompiler::default()
            .compile(&source, profile, compiler_identity())
            .expect("compilation should succeed");
        assert_eq!(
            Vm::default().execute_blu_v1(compiled.into_validated_artifact(), BluLimits::default()),
            Ok(vec![Value::Number(4.0), Value::Number(7.0)])
        );
    }

    for profile in [
        SemanticProfile::Lua51,
        SemanticProfile::Lua52,
        SemanticProfile::Lua53,
        SemanticProfile::Lua54,
        SemanticProfile::Lua55,
    ] {
        assert!(matches!(
            OwnedCompiler::default().compile(&source, profile, compiler_identity()),
            Err(OwnedCompileError::Syntax(_))
        ));
    }

    let once =
        make_source(b"local count = 0\nrepeat count = count + 1 until true\nreturn count".to_vec());
    for profile in SemanticProfile::ALL {
        let compiled = OwnedCompiler::default()
            .compile(&once, profile, compiler_identity())
            .expect("profile-neutral repeat should compile");
        let expected = if matches!(
            profile,
            SemanticProfile::Lua53 | SemanticProfile::Lua54 | SemanticProfile::Lua55
        ) {
            Value::Integer(1)
        } else {
            Value::Number(1.0)
        };
        assert_eq!(
            Vm::default().execute_blu_v1(compiled.into_validated_artifact(), BluLimits::default()),
            Ok(vec![expected]),
            "{profile}"
        );
    }
}

#[test]
fn do_blocks_restore_shadowed_bindings_and_propagate_returns() {
    let scoped = make_source(
        b"local value = 1\ndo\nlocal value = 2\nvalue = value + 3\nend\nreturn value".to_vec(),
    );
    let returning =
        make_source(b"do return 7 end\nlocal unreachable = 9\nreturn unreachable".to_vec());
    for profile in SemanticProfile::ALL {
        let compiled = OwnedCompiler::default()
            .compile(&scoped, profile, compiler_identity())
            .expect("scoped do block should compile");
        let integer_profile = matches!(
            profile,
            SemanticProfile::Lua53 | SemanticProfile::Lua54 | SemanticProfile::Lua55
        );
        let one = if integer_profile {
            Value::Integer(1)
        } else {
            Value::Number(1.0)
        };
        assert_eq!(
            Vm::default().execute_blu_v1(compiled.into_validated_artifact(), BluLimits::default()),
            Ok(vec![one]),
            "{profile}"
        );

        let compiled = OwnedCompiler::default()
            .compile(&returning, profile, compiler_identity())
            .expect("returning do block should compile");
        let seven = if integer_profile {
            Value::Integer(7)
        } else {
            Value::Number(7.0)
        };
        assert_eq!(
            Vm::default().execute_blu_v1(compiled.into_validated_artifact(), BluLimits::default()),
            Ok(vec![seven]),
            "{profile}"
        );
    }
}

#[test]
fn numeric_for_snapshots_bounds_scopes_index_and_supports_loop_control() {
    let source = make_source(
        b"local first = 1\nlocal last = 5\nlocal total = 0\nfor index = first, last do\nlast = 1\nif index == 2 then continue end\nif index == 4 then break end\ntotal = total + index\nend\nreturn total, last"
            .to_vec(),
    );
    for profile in SemanticProfile::ALL {
        let compiled = OwnedCompiler::default().compile(&source, profile, compiler_identity());
        if matches!(profile, SemanticProfile::Blu | SemanticProfile::Luau) {
            assert_eq!(
                Vm::default().execute_blu_v1(
                    compiled.unwrap().into_validated_artifact(),
                    BluLimits::default()
                ),
                Ok(vec![Value::Number(4.0), Value::Number(1.0)]),
                "{profile}"
            );
        } else {
            assert!(matches!(compiled, Err(OwnedCompileError::Syntax(_))));
        }
    }

    let shared = make_source(
        b"local index = 99\nlocal total = 0\nfor index = 1, 3 do total = total + index end\nreturn total, index"
            .to_vec(),
    );
    for profile in SemanticProfile::ALL {
        let compiled = OwnedCompiler::default()
            .compile(&shared, profile, compiler_identity())
            .expect("numeric for should compile");
        let expected = if matches!(
            profile,
            SemanticProfile::Lua53 | SemanticProfile::Lua54 | SemanticProfile::Lua55
        ) {
            vec![Value::Integer(6), Value::Integer(99)]
        } else {
            vec![Value::Number(6.0), Value::Number(99.0)]
        };
        assert_eq!(
            Vm::default().execute_blu_v1(compiled.into_validated_artifact(), BluLimits::default()),
            Ok(expected),
            "{profile}"
        );
    }
}

#[test]
fn numeric_for_accepts_literal_steps_with_profile_specific_zero_direction() {
    let descending = make_source(
        b"local total = 0\nfor index = 5, 1, -2 do total = total + index end\nreturn total"
            .to_vec(),
    );
    let fractional = make_source(
        b"local total = 0\nfor index = 1, 2, 0.5 do total = total + index end\nreturn total"
            .to_vec(),
    );
    for profile in SemanticProfile::ALL {
        let compiled = OwnedCompiler::default()
            .compile(&descending, profile, compiler_identity())
            .expect("negative literal step should compile");
        let expected = if matches!(
            profile,
            SemanticProfile::Lua53 | SemanticProfile::Lua54 | SemanticProfile::Lua55
        ) {
            Value::Integer(9)
        } else {
            Value::Number(9.0)
        };
        assert_eq!(
            Vm::default().execute_blu_v1(compiled.into_validated_artifact(), BluLimits::default()),
            Ok(vec![expected]),
            "{profile}"
        );

        let compiled = OwnedCompiler::default()
            .compile(&fractional, profile, compiler_identity())
            .expect("positive fractional step should compile");
        assert_eq!(
            Vm::default().execute_blu_v1(compiled.into_validated_artifact(), BluLimits::default()),
            Ok(vec![Value::Number(4.5)]),
            "{profile}"
        );
    }

    let zero = make_source(
        b"local count = 0 for index = 1, 0, 0 do count = count + 1 if count == 1 then break end end return count"
            .to_vec(),
    );
    for (profile, expected) in [
        (SemanticProfile::Luau, Value::Number(1.0)),
        (SemanticProfile::Lua51, Value::Number(1.0)),
        (SemanticProfile::Lua52, Value::Number(1.0)),
        (SemanticProfile::Lua53, Value::Integer(1)),
    ] {
        let compiled = OwnedCompiler::default()
            .compile(&zero, profile, compiler_identity())
            .expect("profile assigns zero-step direction");
        assert_eq!(
            Vm::default().execute_blu_v1(compiled.into_validated_artifact(), BluLimits::default()),
            Ok(vec![expected]),
            "{profile}"
        );
    }

    for profile in [
        SemanticProfile::Blu,
        SemanticProfile::Lua54,
        SemanticProfile::Lua55,
    ] {
        let source = make_source(b"for index = 1, 3, 0 do end".to_vec());
        let error = OwnedCompiler::default()
            .compile(&source, profile, compiler_identity())
            .expect_err("rejected zero-step semantics should fail explicitly");
        assert!(matches!(error, OwnedCompileError::Diagnostic(_)));
    }

    let dynamic = make_source(b"local step = 1\nfor index = 1, 3, step do end".to_vec());
    let error = OwnedCompiler::default()
        .compile(&dynamic, SemanticProfile::Blu, compiler_identity())
        .expect_err("dynamic step direction should fail explicitly");
    assert!(matches!(error, OwnedCompileError::Diagnostic(_)));
}

#[test]
fn numeric_for_snapshots_dynamic_steps_for_profiles_with_assigned_zero_behavior() {
    let dynamic = make_source(
        b"local calls = 0 local function getstep() calls = calls + 1 return -2 end local total = 0 for index = 5, 1, getstep() do total = total + index end return total, calls"
            .to_vec(),
    );
    let zero = make_source(
        b"local step = 0 local count = 0 for index = 1, 0, step do count = count + 1 if count == 1 then break end end return count"
            .to_vec(),
    );
    for profile in [
        SemanticProfile::Luau,
        SemanticProfile::Lua51,
        SemanticProfile::Lua52,
        SemanticProfile::Lua53,
    ] {
        let compiled = OwnedCompiler::default()
            .compile(&dynamic, profile, compiler_identity())
            .expect("profile assigns dynamic step direction");
        let expected = if profile == SemanticProfile::Lua53 {
            vec![Value::Integer(9), Value::Integer(1)]
        } else {
            vec![Value::Number(9.0), Value::Number(1.0)]
        };
        assert_eq!(
            Vm::default().execute_blu_v1(compiled.into_validated_artifact(), BluLimits::default()),
            Ok(expected),
            "{profile}"
        );

        let compiled = OwnedCompiler::default()
            .compile(&zero, profile, compiler_identity())
            .expect("profile assigns dynamic zero-step direction");
        let expected = if profile == SemanticProfile::Lua53 {
            Value::Integer(1)
        } else {
            Value::Number(1.0)
        };
        assert_eq!(
            Vm::default().execute_blu_v1(compiled.into_validated_artifact(), BluLimits::default()),
            Ok(vec![expected]),
            "{profile}"
        );
    }

    for profile in [
        SemanticProfile::Blu,
        SemanticProfile::Lua54,
        SemanticProfile::Lua55,
    ] {
        let error = OwnedCompiler::default()
            .compile(&dynamic, profile, compiler_identity())
            .expect_err("dynamic zero behavior is not executable for this profile");
        assert!(matches!(error, OwnedCompileError::Diagnostic(_)));
    }
}

#[test]
fn generic_for_executes_pairs_and_owned_iterators_across_profiles() {
    for (bytes, result) in [
        (
            b"local total = 0 for key, value in pairs({ left = 2, right = 3 }) do total = total + value end return total"
                .as_slice(),
            5,
        ),
        (
            b"local function iterator(limit, control) local value = control + 1 if value <= limit then return value, value * 2 end end local total = 0 for key, value in iterator, 3, 0 do total = total + key + value end return total"
                .as_slice(),
            18,
        ),
        (
            b"local count = 0 local function iterator() count = count + 1 if count == 1 then return false, 42 end end local total = 0 for key, value in iterator do total = total + value end return total"
                .as_slice(),
            42,
        ),
    ] {
        let source = make_source(bytes.to_vec());
        for profile in SemanticProfile::ALL {
            let compiled = OwnedCompiler::default().compile(&source, profile, compiler_identity());
            if matches!(profile, SemanticProfile::Lua54 | SemanticProfile::Lua55) {
                assert!(
                    matches!(compiled, Err(OwnedCompileError::Diagnostic(_))),
                    "{profile}"
                );
                continue;
            }
            let result = if profile == SemanticProfile::Lua53 {
                Value::Integer(result)
            } else {
                Value::Number(result as f64)
            };
            assert_eq!(
                Vm::default().execute_blu_v1(
                    compiled.unwrap().into_validated_artifact(),
                    BluLimits::default()
                ),
                Ok(vec![result]),
                "{profile}"
            );
        }
    }
}

#[test]
fn generic_for_adjusts_the_final_control_call_exactly_once() {
    let source = make_source(
        b"local calls = 0 local function iterator(limit, control) local value = control + 1 if value <= limit then return value, value end end local function controls() calls = calls + 1 return iterator, 3, 0 end local total = 0 for key, value in controls() do total = total + value end return total, calls"
            .to_vec(),
    );
    for profile in SemanticProfile::ALL {
        let compiled = OwnedCompiler::default().compile(&source, profile, compiler_identity());
        if matches!(profile, SemanticProfile::Lua54 | SemanticProfile::Lua55) {
            assert!(matches!(compiled, Err(OwnedCompileError::Diagnostic(_))));
            continue;
        }
        let integer_profile = profile == SemanticProfile::Lua53;
        assert_eq!(
            Vm::default().execute_blu_v1(
                compiled.unwrap().into_validated_artifact(),
                BluLimits::default()
            ),
            Ok(vec![
                if integer_profile {
                    Value::Integer(6)
                } else {
                    Value::Number(6.0)
                },
                if integer_profile {
                    Value::Integer(1)
                } else {
                    Value::Number(1.0)
                },
            ]),
            "{profile}"
        );
    }
}

#[test]
fn owned_scalar_globals_round_trip_and_persist_in_the_vm_registry() {
    let source = make_source(b"answer = 40\nanswer = answer + 2\nreturn answer, missing".to_vec());
    for profile in SemanticProfile::ALL {
        let compiled = OwnedCompiler::default()
            .compile(&source, profile, compiler_identity())
            .expect("global source should compile");
        assert!(
            compiled
                .artifact()
                .main()
                .required_features
                .contains(FeatureBits::GLOBALS)
        );
        let mut vm = Vm::default();
        let expected = if matches!(
            profile,
            SemanticProfile::Lua53 | SemanticProfile::Lua54 | SemanticProfile::Lua55
        ) {
            Value::Integer(42)
        } else {
            Value::Number(42.0)
        };
        assert_eq!(
            vm.execute_blu_v1(compiled.into_validated_artifact(), BluLimits::default()),
            Ok(vec![expected.clone(), Value::Nil]),
            "{profile}"
        );
        assert_eq!(vm.global(b"answer"), Some(&expected));
    }
}

#[test]
fn owned_empty_tables_support_indexed_reads_writes_and_missing_keys() {
    let source = make_source(
        br#"local values = {}; values["answer"] = 40; return values["answer"], values["missing"]"#
            .to_vec(),
    );
    for profile in SemanticProfile::ALL {
        let compiled = OwnedCompiler::default()
            .compile(&source, profile, compiler_identity())
            .expect("table source should compile");
        assert!(
            compiled
                .artifact()
                .main()
                .required_features
                .contains(FeatureBits::TABLES)
        );
        assert!(
            compiled
                .artifact()
                .main()
                .code
                .iter()
                .any(|instruction| matches!(instruction, Instruction::NewTable { .. }))
        );
        assert!(
            compiled
                .artifact()
                .main()
                .code
                .iter()
                .any(|instruction| matches!(instruction, Instruction::SetTable { .. }))
        );
        assert!(
            compiled
                .artifact()
                .main()
                .code
                .iter()
                .any(|instruction| matches!(instruction, Instruction::GetTable { .. }))
        );
        let expected = if matches!(
            profile,
            SemanticProfile::Lua53 | SemanticProfile::Lua54 | SemanticProfile::Lua55
        ) {
            Value::Integer(40)
        } else {
            Value::Number(40.0)
        };
        assert_eq!(
            Vm::default().execute_blu_v1(compiled.into_validated_artifact(), BluLimits::default()),
            Ok(vec![expected, Value::Nil]),
            "{profile}"
        );
    }
}

#[test]
fn owned_table_fields_and_dot_sugar_preserve_source_order() {
    let source = make_source(
        br#"local values = {10, answer = 20, ["other"] = 30, answer = 21}; values.other = 31; return values[1], values.answer, values.other"#
            .to_vec(),
    );
    for profile in SemanticProfile::ALL {
        let compiled = OwnedCompiler::default()
            .compile(&source, profile, compiler_identity())
            .expect("table fields should compile");
        let integer_profile = matches!(
            profile,
            SemanticProfile::Lua53 | SemanticProfile::Lua54 | SemanticProfile::Lua55
        );
        let value = |integer, number| {
            if integer_profile {
                Value::Integer(integer)
            } else {
                Value::Number(number)
            }
        };
        assert_eq!(
            Vm::default().execute_blu_v1(compiled.into_validated_artifact(), BluLimits::default()),
            Ok(vec![value(10, 10.0), value(21, 21.0), value(31, 31.0)]),
            "{profile}"
        );
    }
}

#[test]
fn owned_fixed_calls_use_globals_fields_scalar_adjustment_and_output() {
    let source =
        make_source(br#"print("captured"); return string.sub("blue", 2), type({})"#.to_vec());
    for profile in SemanticProfile::ALL {
        let compiled = OwnedCompiler::default()
            .compile(&source, profile, compiler_identity())
            .expect("fixed calls should compile");
        assert!(
            compiled
                .artifact()
                .main()
                .required_features
                .contains(FeatureBits::FIXED_CALLS)
        );
        let mut vm = Vm::default();
        assert_eq!(
            vm.execute_blu_v1(compiled.into_validated_artifact(), BluLimits::default()),
            Ok(vec![
                Value::String(b"lue".as_slice().into()),
                Value::String(b"table".as_slice().into()),
            ]),
            "{profile}"
        );
        assert_eq!(vm.take_output(), b"captured\n", "{profile}");
    }
}

#[test]
fn owned_fixed_calls_propagate_native_errors() {
    let source = make_source(br#"return error("owned failure")"#.to_vec());
    let compiled = OwnedCompiler::default()
        .compile(&source, SemanticProfile::Blu, compiler_identity())
        .unwrap();
    assert!(matches!(
        Vm::default().execute_blu_v1(
            compiled.into_validated_artifact(),
            BluLimits::default()
        ),
        Err(blu_runtime::RuntimeError::Raised(Value::String(message)))
            if message.as_ref() == b"owned failure"
    ));
}

#[test]
fn owned_fixed_calls_reach_host_registered_globals() {
    let source = make_source(br#"return host_echo("registered")"#.to_vec());
    let compiled = OwnedCompiler::default()
        .compile(&source, SemanticProfile::Blu, compiler_identity())
        .unwrap();
    let mut vm = Vm::default();
    let echo = vm.register_function(|_, arguments| Ok(arguments.to_vec()));
    vm.set_global(b"host_echo".as_slice(), Value::NativeFunction(echo));
    assert_eq!(
        vm.execute_blu_v1(compiled.into_validated_artifact(), BluLimits::default()),
        Ok(vec![Value::String(b"registered".as_slice().into())])
    );
}

#[test]
fn owned_method_calls_pass_receiver_once_before_explicit_arguments() {
    let source = make_source(
        br#"local object = {method = host_second}; return object:method("registered")"#.to_vec(),
    );
    for profile in SemanticProfile::ALL {
        let compiled = OwnedCompiler::default()
            .compile(&source, profile, compiler_identity())
            .expect("method calls should compile");
        let mut vm = Vm::default();
        let second = vm.register_function(|_, arguments| {
            assert!(matches!(arguments.first(), Some(Value::Table(_))));
            Ok(vec![arguments.get(1).cloned().unwrap_or(Value::Nil)])
        });
        vm.set_global(b"host_second".as_slice(), Value::NativeFunction(second));
        assert_eq!(
            vm.execute_blu_v1(compiled.into_validated_artifact(), BluLimits::default()),
            Ok(vec![Value::String(b"registered".as_slice().into())]),
            "{profile}"
        );
    }
}

#[test]
fn owned_callable_tables_resume_across_call_result_forms() {
    let source = make_source(
        b"local callable callable = setmetatable({}, {__call = function(self, first, ...) return self == callable, first, ... end}) local scalar = callable(3) local a, b, c = callable(4, 5) local list = {callable(6, 7)} local function forward(...) return callable(...) end return scalar, a, b, c, list[1], list[2], list[3], forward(8, 9)"
            .to_vec(),
    );
    for profile in SemanticProfile::ALL {
        let compiled = OwnedCompiler::default()
            .compile(&source, profile, compiler_identity())
            .unwrap();
        let number = |value| {
            if matches!(
                profile,
                SemanticProfile::Lua53 | SemanticProfile::Lua54 | SemanticProfile::Lua55
            ) {
                Value::Integer(value)
            } else {
                Value::Number(value as f64)
            }
        };
        assert_eq!(
            Vm::default().execute_blu_v1(compiled.into_validated_artifact(), BluLimits::default()),
            Ok(vec![
                Value::Boolean(true),
                Value::Boolean(true),
                number(4),
                number(5),
                Value::Boolean(true),
                number(6),
                number(7),
                Value::Boolean(true),
                number(8),
                number(9),
            ]),
            "{profile}"
        );
    }
}

#[test]
fn owned_callable_table_chains_prepend_each_receiver() {
    let source = make_source(
        b"local outer local middle middle = setmetatable({}, {__call = function(second, first, value) return second == middle, first == outer, value end}) outer = setmetatable({}, {__call = middle}) return outer(9)"
            .to_vec(),
    );
    for profile in SemanticProfile::ALL {
        let compiled = OwnedCompiler::default()
            .compile(&source, profile, compiler_identity())
            .unwrap();
        let nine = if matches!(
            profile,
            SemanticProfile::Lua53 | SemanticProfile::Lua54 | SemanticProfile::Lua55
        ) {
            Value::Integer(9)
        } else {
            Value::Number(9.0)
        };
        assert_eq!(
            Vm::default().execute_blu_v1(compiled.into_validated_artifact(), BluLimits::default()),
            Ok(vec![Value::Boolean(true), Value::Boolean(true), nine]),
            "{profile}"
        );
    }
}

#[test]
fn owned_callable_table_cycles_fail_at_the_profile_bound() {
    let source = make_source(
        b"local value = {} setmetatable(value, {__call = value}) return value()".to_vec(),
    );
    for profile in SemanticProfile::ALL {
        let compiled = OwnedCompiler::default()
            .compile(&source, profile, compiler_identity())
            .unwrap();
        assert_eq!(
            Vm::default().execute_blu_v1(compiled.into_validated_artifact(), BluLimits::default()),
            Err(RuntimeError::MetatableLoop),
            "{profile}"
        );
    }
}

#[test]
fn owned_indexing_non_tables_returns_structured_type_errors() {
    for bytes in [
        br#"local value = 1; return value["key"]"#.as_slice(),
        br#"local value = 1; value["key"] = 2"#.as_slice(),
    ] {
        let source = make_source(bytes.to_vec());
        let compiled = OwnedCompiler::default()
            .compile(&source, SemanticProfile::Blu, compiler_identity())
            .unwrap();
        assert!(matches!(
            Vm::default().execute_blu_v1(compiled.into_validated_artifact(), BluLimits::default()),
            Err(blu_runtime::RuntimeError::Type { .. })
        ));
    }
}

#[test]
fn ordered_comparisons_reject_incompatible_operand_types() {
    let source = make_source(br#"return 1 < "2""#.to_vec());
    for profile in SemanticProfile::ALL {
        let compiled = OwnedCompiler::default()
            .compile(&source, profile, compiler_identity())
            .unwrap();
        assert!(
            Vm::default()
                .execute_blu_v1(compiled.into_validated_artifact(), BluLimits::default())
                .is_err(),
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
fn owned_table_length_is_raw_or_invokes_a_resumable_metamethod() {
    let raw = make_source(b"return #{1, 2, 3}, #{}".to_vec());
    for profile in SemanticProfile::ALL {
        let compiled = OwnedCompiler::default()
            .compile(&raw, profile, compiler_identity())
            .unwrap();
        let modern = matches!(
            profile,
            SemanticProfile::Lua53 | SemanticProfile::Lua54 | SemanticProfile::Lua55
        );
        assert_eq!(
            Vm::default().execute_blu_v1(compiled.into_validated_artifact(), BluLimits::default()),
            Ok(vec![
                if modern {
                    Value::Integer(3)
                } else {
                    Value::Number(3.0)
                },
                if modern {
                    Value::Integer(0)
                } else {
                    Value::Number(0.0)
                },
            ]),
            "{profile}"
        );
    }

    let metamethod = make_source(
        b"local value = setmetatable({1, 2}, {__len = function() return 9 end}) return #value"
            .to_vec(),
    );
    for profile in SemanticProfile::ALL {
        let compiled = OwnedCompiler::default()
            .compile(&metamethod, profile, compiler_identity())
            .unwrap();
        let result =
            Vm::default().execute_blu_v1(compiled.into_validated_artifact(), BluLimits::default());
        if profile == SemanticProfile::Lua51 {
            assert_eq!(result, Ok(vec![Value::Number(2.0)]));
        } else if matches!(
            profile,
            SemanticProfile::Lua53 | SemanticProfile::Lua54 | SemanticProfile::Lua55
        ) {
            assert_eq!(result, Ok(vec![Value::Integer(9)]));
        } else {
            assert_eq!(result, Ok(vec![Value::Number(9.0)]));
        }
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
fn binary_integers_lower_only_for_blu_and_luau() {
    let source = make_source(b"return 0b101010, 0B1111_0000".to_vec());
    for profile in SemanticProfile::ALL {
        let result = OwnedCompiler::default().compile(&source, profile, compiler_identity());
        if matches!(profile, SemanticProfile::Blu | SemanticProfile::Luau) {
            let compiled = result.unwrap();
            assert_eq!(
                compiled.artifact().prototypes()[0].constants.as_slice(),
                [Constant::Number(42.0), Constant::Number(240.0)],
                "{profile}"
            );
        } else {
            let error = result.unwrap_err();
            let rejected = error.syntax().expect("binary integer rejection");
            assert_eq!(
                rejected.diagnostics()[0].code().as_str(),
                "BLU-LEX-0014",
                "{profile}"
            );
        }
    }
}

#[test]
fn hexadecimal_numbers_follow_the_profile_matrix() {
    let shared = make_source(b"return 0x1p2, 0x1p-2".to_vec());
    for profile in SemanticProfile::ALL {
        let result = OwnedCompiler::default().compile(&shared, profile, compiler_identity());
        if profile == SemanticProfile::Luau {
            let rejected = result.unwrap_err();
            assert_eq!(
                rejected.syntax().unwrap().diagnostics()[0].code().as_str(),
                "BLU-LEX-0016"
            );
        } else {
            let compiled = result.unwrap();
            assert_eq!(
                compiled.artifact().prototypes()[0].constants.as_slice(),
                [Constant::Number(4.0), Constant::Number(0.25)],
                "{profile}"
            );
        }
    }

    let fractional = make_source(b"return 0x1.8p1, 0x.8p1, 0x1.8".to_vec());
    for profile in SemanticProfile::ALL {
        let result = OwnedCompiler::default().compile(&fractional, profile, compiler_identity());
        if matches!(profile, SemanticProfile::Luau | SemanticProfile::Lua51) {
            assert_eq!(
                result.unwrap_err().syntax().unwrap().diagnostics()[0]
                    .code()
                    .as_str(),
                "BLU-LEX-0016",
                "{profile}"
            );
        } else {
            let compiled = result.unwrap();
            assert_eq!(
                compiled.artifact().prototypes()[0].constants.as_slice(),
                [
                    Constant::Number(3.0),
                    Constant::Number(1.0),
                    Constant::Number(1.5),
                ],
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

    let source = make_source(b"return [=[\nabc]=]".to_vec());
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
}

#[test]
fn long_strings_follow_explicit_lua_and_luau_newline_semantics() {
    let source = make_source(b"return [==[\ra\rb\r\nc\\n\0\xff]==]".to_vec());
    for profile in SemanticProfile::ALL {
        let compiled = OwnedCompiler::default()
            .compile(&source, profile, compiler_identity())
            .unwrap();
        let expected = if profile == SemanticProfile::Luau {
            b"\ra\rb\nc\\n\0\xff".as_slice()
        } else {
            b"a\nb\nc\\n\0\xff".as_slice()
        };
        assert_eq!(
            compiled.artifact().main().constants,
            [Constant::String(expected.to_vec())],
            "{profile}"
        );
    }
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
fn string_find_uses_byte_indices_plain_search_and_profile_subtypes() {
    let source = make_source(
        b"local a, b = string.find('a\\000bc\\000b', '\\000b', -4, true) local c, d = string.find('abc', '', 2) local missing = string.find('abc', 'z') return a, b, c, d, missing"
            .to_vec(),
    );
    for profile in SemanticProfile::ALL {
        let compiled = OwnedCompiler::default()
            .compile(&source, profile, compiler_identity())
            .unwrap();
        let index = |value| {
            if matches!(
                profile,
                SemanticProfile::Blu
                    | SemanticProfile::Lua53
                    | SemanticProfile::Lua54
                    | SemanticProfile::Lua55
            ) {
                Value::Integer(value)
            } else {
                Value::Number(value as f64)
            }
        };
        assert_eq!(
            Vm::default().execute_blu_v1(compiled.into_validated_artifact(), BluLimits::default()),
            Ok(vec![index(5), index(6), index(2), index(1), Value::Nil]),
            "{profile}"
        );
    }
}

#[test]
fn string_find_plain_mode_remains_literal() {
    for profile in SemanticProfile::ALL {
        let compiled = OwnedCompiler::default()
            .compile(
                &make_source(b"return string.find('a.c', '.', 1, true)".to_vec()),
                profile,
                compiler_identity(),
            )
            .unwrap();
        let result =
            Vm::default().execute_blu_v1(compiled.into_validated_artifact(), BluLimits::default());
        assert!(
            matches!(result, Ok(values) if values.len() == 2),
            "{profile}"
        );
    }
}

#[test]
fn string_find_supports_greedy_optional_and_minimal_repetition() {
    let source = make_source(
        b"local a,b=string.find('xxaaab','a*b') local c,d=string.find('xxb','a?b') local e,f=string.find('aaaa','a*a') local g,h=string.find('aaaa','a-a') local missing=string.find('xxb','a+b') return a,b,c,d,e,f,g,h,missing"
            .to_vec(),
    );
    for profile in SemanticProfile::ALL {
        let compiled = OwnedCompiler::default()
            .compile(&source, profile, compiler_identity())
            .unwrap();
        let index = |value| {
            if matches!(
                profile,
                SemanticProfile::Blu
                    | SemanticProfile::Lua53
                    | SemanticProfile::Lua54
                    | SemanticProfile::Lua55
            ) {
                Value::Integer(value)
            } else {
                Value::Number(value as f64)
            }
        };
        assert_eq!(
            Vm::default().execute_blu_v1(compiled.into_validated_artifact(), BluLimits::default()),
            Ok(vec![
                index(3),
                index(6),
                index(3),
                index(3),
                index(1),
                index(4),
                index(1),
                index(1),
                Value::Nil
            ]),
            "{profile}"
        );
    }
}

#[test]
fn string_find_rejects_malformed_repetition_structurally() {
    for profile in SemanticProfile::ALL {
        let source = make_source(b"return string.find('abc', '*a')".to_vec());
        let compiled = OwnedCompiler::default()
            .compile(&source, profile, compiler_identity())
            .unwrap();
        assert!(matches!(
            Vm::default().execute_blu_v1(compiled.into_validated_artifact(), BluLimits::default()),
            Err(RuntimeError::UnsupportedLibraryFeature {
                function: "string.find",
                feature: "malformed Lua pattern repetition",
            })
        ));
    }
}

#[test]
fn string_find_supports_basic_anchors_wildcards_and_escapes() {
    let source = make_source(
        b"local a, b = string.find('xxa.c', '^a.c$', 3) local c, d = string.find('xxa.c', 'a%.c$') return a, b, c, d"
            .to_vec(),
    );
    for profile in SemanticProfile::ALL {
        let compiled = OwnedCompiler::default()
            .compile(&source, profile, compiler_identity())
            .unwrap();
        let index = |value| {
            if matches!(
                profile,
                SemanticProfile::Blu
                    | SemanticProfile::Lua53
                    | SemanticProfile::Lua54
                    | SemanticProfile::Lua55
            ) {
                Value::Integer(value)
            } else {
                Value::Number(value as f64)
            }
        };
        assert_eq!(
            Vm::default().execute_blu_v1(compiled.into_validated_artifact(), BluLimits::default()),
            Ok(vec![index(3), index(5), index(3), index(5)]),
            "{profile}"
        );
    }
}

#[test]
fn string_find_supports_common_byte_classes_and_negation() {
    let source = make_source(
        b"local a, b = string.find('x7Y', '%d') local c, d = string.find('x7Y', '%D') local e, f = string.find('\\000a', '%z') return a, b, c, d, e, f"
            .to_vec(),
    );
    for profile in SemanticProfile::ALL {
        let compiled = OwnedCompiler::default()
            .compile(&source, profile, compiler_identity())
            .unwrap();
        let index = |value| {
            if matches!(
                profile,
                SemanticProfile::Blu
                    | SemanticProfile::Lua53
                    | SemanticProfile::Lua54
                    | SemanticProfile::Lua55
            ) {
                Value::Integer(value)
            } else {
                Value::Number(value as f64)
            }
        };
        assert_eq!(
            Vm::default().execute_blu_v1(compiled.into_validated_artifact(), BluLimits::default()),
            Ok(vec![
                index(2),
                index(2),
                index(1),
                index(1),
                index(1),
                index(1)
            ]),
            "{profile}"
        );
    }
}

#[test]
fn string_graph_classes_follow_the_explicit_lua51_split() {
    let source = make_source(
        b"local a=string.match(' A!','%g+') local b=string.match('A !','%G+') local c=string.match(' A!','[%g]+') return a,b,c"
            .to_vec(),
    );
    for profile in SemanticProfile::ALL {
        let compiled = OwnedCompiler::default()
            .compile(&source, profile, compiler_identity())
            .unwrap();
        let expected = if profile == SemanticProfile::Lua51 {
            vec![Value::Nil, Value::Nil, Value::Nil]
        } else {
            vec![
                Value::String(Arc::from(&b"A!"[..])),
                Value::String(Arc::from(&b" "[..])),
                Value::String(Arc::from(&b"A!"[..])),
            ]
        };
        assert_eq!(
            Vm::default().execute_blu_v1(compiled.into_validated_artifact(), BluLimits::default()),
            Ok(expected),
            "{profile}"
        );
    }
}

#[test]
fn string_find_rejects_nonportable_pattern_classes_explicitly() {
    for profile in SemanticProfile::ALL {
        let source = make_source(b"return string.find('!', '%q')".to_vec());
        let compiled = OwnedCompiler::default()
            .compile(&source, profile, compiler_identity())
            .unwrap();
        assert!(matches!(
            Vm::default().execute_blu_v1(compiled.into_validated_artifact(), BluLimits::default()),
            Err(RuntimeError::UnsupportedLibraryFeature {
                function: "string.find",
                feature: "dialect-specific Lua pattern classes and captures",
            })
        ));
    }
}

#[test]
fn string_find_supports_sets_ranges_classes_and_negation() {
    let source = make_source(
        b"local a, b = string.find('0bZ]', '[a-c]') local c, d = string.find('0bZ]', '[^%d]') local e, f = string.find('0bZ]', '[]]') return a, b, c, d, e, f"
            .to_vec(),
    );
    for profile in SemanticProfile::ALL {
        let compiled = OwnedCompiler::default()
            .compile(&source, profile, compiler_identity())
            .unwrap();
        let index = |value| {
            if matches!(
                profile,
                SemanticProfile::Blu
                    | SemanticProfile::Lua53
                    | SemanticProfile::Lua54
                    | SemanticProfile::Lua55
            ) {
                Value::Integer(value)
            } else {
                Value::Number(value as f64)
            }
        };
        assert_eq!(
            Vm::default().execute_blu_v1(compiled.into_validated_artifact(), BluLimits::default()),
            Ok(vec![
                index(2),
                index(2),
                index(2),
                index(2),
                index(4),
                index(4)
            ]),
            "{profile}"
        );
    }
}

#[test]
fn string_find_rejects_malformed_sets_structurally() {
    for profile in SemanticProfile::ALL {
        let source = make_source(b"return string.find('abc', '[abc')".to_vec());
        let compiled = OwnedCompiler::default()
            .compile(&source, profile, compiler_identity())
            .unwrap();
        assert!(matches!(
            Vm::default().execute_blu_v1(compiled.into_validated_artifact(), BluLimits::default()),
            Err(RuntimeError::UnsupportedLibraryFeature {
                function: "string.find",
                feature: "malformed Lua pattern sets",
            })
        ));
    }
}

#[test]
fn string_find_preflights_pattern_work() {
    let source = make_source(
        b"return string.find(string.rep('a', 4000), string.rep('a', 3000) .. 'b')".to_vec(),
    );
    for profile in SemanticProfile::ALL {
        let compiled = OwnedCompiler::default()
            .compile(&source, profile, compiler_identity())
            .unwrap();
        assert!(matches!(
            Vm::default().execute_blu_v1(compiled.into_validated_artifact(), BluLimits::default()),
            Err(RuntimeError::PatternWorkLimit {
                limit: 10_000_000,
                ..
            })
        ));
    }
}

#[test]
fn string_match_returns_bounded_byte_matches_and_nil_misses() {
    let source = make_source(
        b"local a=string.match('xxabbbc','a[b]+c') local b=string.match('abc','',2) local c=string.match('abc','%d') local d=string.match('abc','a',5) return a,b,c,d"
            .to_vec(),
    );
    for profile in SemanticProfile::ALL {
        let compiled = OwnedCompiler::default()
            .compile(&source, profile, compiler_identity())
            .unwrap();
        assert_eq!(
            Vm::default().execute_blu_v1(compiled.into_validated_artifact(), BluLimits::default()),
            Ok(vec![
                Value::String(Arc::from(&b"abbbc"[..])),
                Value::String(Arc::from(&b""[..])),
                Value::Nil,
                Value::Nil
            ]),
            "{profile}"
        );
    }
}

#[test]
fn string_match_shares_structured_pattern_and_argument_errors() {
    for profile in SemanticProfile::ALL {
        for (source, expected) in [
            (b"return string.match('abc', '%1')".as_slice(), "pattern"),
            (b"return string.match({}, 'a')".as_slice(), "type"),
        ] {
            let source = make_source(source.to_vec());
            let compiled = OwnedCompiler::default()
                .compile(&source, profile, compiler_identity())
                .unwrap();
            let result = Vm::default()
                .execute_blu_v1(compiled.into_validated_artifact(), BluLimits::default());
            match expected {
                "pattern" => assert!(matches!(
                    result,
                    Err(RuntimeError::UnsupportedLibraryFeature { .. })
                )),
                "type" => assert!(matches!(result, Err(RuntimeError::Type { .. }))),
                _ => unreachable!(),
            }
        }
    }
}

#[test]
fn string_find_and_match_return_nested_and_position_captures() {
    let source = make_source(
        b"local a,b,c,d,e=string.find('xxab12','(a(b))(%d+)') local f,g,h=string.match('xxab12','(a(b))(%d+)') local i,j=string.match('abc','()b()') return a,b,c,d,e,f,g,h,i,j"
            .to_vec(),
    );
    for profile in SemanticProfile::ALL {
        let compiled = OwnedCompiler::default()
            .compile(&source, profile, compiler_identity())
            .unwrap();
        let index = |value| {
            if matches!(
                profile,
                SemanticProfile::Blu
                    | SemanticProfile::Lua53
                    | SemanticProfile::Lua54
                    | SemanticProfile::Lua55
            ) {
                Value::Integer(value)
            } else {
                Value::Number(value as f64)
            }
        };
        assert_eq!(
            Vm::default().execute_blu_v1(compiled.into_validated_artifact(), BluLimits::default()),
            Ok(vec![
                index(3),
                index(6),
                Value::String(Arc::from(&b"ab"[..])),
                Value::String(Arc::from(&b"b"[..])),
                Value::String(Arc::from(&b"12"[..])),
                Value::String(Arc::from(&b"ab"[..])),
                Value::String(Arc::from(&b"b"[..])),
                Value::String(Arc::from(&b"12"[..])),
                index(2),
                index(3)
            ]),
            "{profile}"
        );
    }
}

#[test]
fn string_patterns_match_completed_substring_backreferences() {
    let source = make_source(
        b"local a,b,c=string.find('xxabab','(ab)%1') local d=string.match('abac','(ab)%1') local e=string.match('x','(a*)%1') local f=string.match('abc','()%1') return a,b,c,d,e,f"
            .to_vec(),
    );
    for profile in SemanticProfile::ALL {
        let compiled = OwnedCompiler::default()
            .compile(&source, profile, compiler_identity())
            .unwrap();
        let index = |value| {
            if matches!(
                profile,
                SemanticProfile::Blu
                    | SemanticProfile::Lua53
                    | SemanticProfile::Lua54
                    | SemanticProfile::Lua55
            ) {
                Value::Integer(value)
            } else {
                Value::Number(value as f64)
            }
        };
        assert_eq!(
            Vm::default().execute_blu_v1(compiled.into_validated_artifact(), BluLimits::default()),
            Ok(vec![
                index(3),
                index(6),
                Value::String(Arc::from(&b"ab"[..])),
                Value::Nil,
                Value::String(Arc::from(&b""[..])),
                Value::Nil,
            ]),
            "{profile}"
        );
    }
}

#[test]
fn string_patterns_reject_invalid_backreferences_structurally() {
    for profile in SemanticProfile::ALL {
        for source in [
            b"return string.match('abc', '%1')".as_slice(),
            b"return string.match('abc', '(a)%2')".as_slice(),
        ] {
            let source = make_source(source.to_vec());
            let compiled = OwnedCompiler::default()
                .compile(&source, profile, compiler_identity())
                .unwrap();
            assert!(matches!(
                Vm::default()
                    .execute_blu_v1(compiled.into_validated_artifact(), BluLimits::default()),
                Err(RuntimeError::UnsupportedLibraryFeature {
                    feature: "invalid Lua pattern capture reference",
                    ..
                })
            ));
        }
    }
}

#[test]
fn string_patterns_match_balanced_byte_pairs() {
    let source = make_source(
        b"local a,b=string.find('x(a(b)c)y','%b()') local c=string.match('x<a<b>c>y','%b<>') local d=string.match('(abc','%b()') return a,b,c,d"
            .to_vec(),
    );
    for profile in SemanticProfile::ALL {
        let compiled = OwnedCompiler::default()
            .compile(&source, profile, compiler_identity())
            .unwrap();
        let index = |value| {
            if matches!(
                profile,
                SemanticProfile::Blu
                    | SemanticProfile::Lua53
                    | SemanticProfile::Lua54
                    | SemanticProfile::Lua55
            ) {
                Value::Integer(value)
            } else {
                Value::Number(value as f64)
            }
        };
        assert_eq!(
            Vm::default().execute_blu_v1(compiled.into_validated_artifact(), BluLimits::default()),
            Ok(vec![
                index(2),
                index(8),
                Value::String(Arc::from(&b"<a<b>c>"[..])),
                Value::Nil,
            ]),
            "{profile}"
        );
    }
}

#[test]
fn string_patterns_reject_malformed_balanced_atoms_structurally() {
    for profile in SemanticProfile::ALL {
        let source = make_source(b"return string.match('abc', '%b(')".to_vec());
        let compiled = OwnedCompiler::default()
            .compile(&source, profile, compiler_identity())
            .unwrap();
        assert!(matches!(
            Vm::default().execute_blu_v1(compiled.into_validated_artifact(), BluLimits::default()),
            Err(RuntimeError::UnsupportedLibraryFeature {
                function: "string.match",
                feature: "malformed Lua balanced patterns",
            })
        ));
    }
}

#[test]
fn string_patterns_match_zero_width_frontiers() {
    let source = make_source(
        b"local a,b=string.find('hello world','%f[%a]world') local c=string.match('123abc','%f[%a]%a+') local d,e=string.find('abc','%f[%z]') local f=string.match('abc','%f[%d]') return a,b,c,d,e,f"
            .to_vec(),
    );
    for profile in SemanticProfile::ALL {
        let compiled = OwnedCompiler::default()
            .compile(&source, profile, compiler_identity())
            .unwrap();
        let index = |value| {
            if matches!(
                profile,
                SemanticProfile::Blu
                    | SemanticProfile::Lua53
                    | SemanticProfile::Lua54
                    | SemanticProfile::Lua55
            ) {
                Value::Integer(value)
            } else {
                Value::Number(value as f64)
            }
        };
        assert_eq!(
            Vm::default().execute_blu_v1(compiled.into_validated_artifact(), BluLimits::default()),
            Ok(vec![
                index(7),
                index(11),
                Value::String(Arc::from(&b"abc"[..])),
                index(4),
                index(3),
                Value::Nil,
            ]),
            "{profile}"
        );
    }
}

#[test]
fn string_patterns_reject_malformed_frontiers_structurally() {
    for profile in SemanticProfile::ALL {
        for pattern in [b"%f".as_slice(), b"%fa".as_slice()] {
            let mut source = b"return string.match('abc', '".to_vec();
            source.extend_from_slice(pattern);
            source.extend_from_slice(b"')");
            let compiled = OwnedCompiler::default()
                .compile(&make_source(source), profile, compiler_identity())
                .unwrap();
            assert!(matches!(
                Vm::default()
                    .execute_blu_v1(compiled.into_validated_artifact(), BluLimits::default()),
                Err(RuntimeError::UnsupportedLibraryFeature {
                    function: "string.match",
                    feature: "malformed Lua frontier patterns",
                })
            ));
        }
    }
}

#[test]
fn string_gsub_replaces_bounded_matches_and_reports_profile_typed_counts() {
    let source = make_source(
        b"local a,b=string.gsub('abc123','%d','[%0]') local c,d=string.gsub('abc','','-') local e,f=string.gsub('aaaa','a',7,2) return a,b,c,d,e,f"
            .to_vec(),
    );
    for profile in SemanticProfile::ALL {
        let compiled = OwnedCompiler::default()
            .compile(&source, profile, compiler_identity())
            .unwrap();
        let count = |value| {
            if matches!(
                profile,
                SemanticProfile::Blu
                    | SemanticProfile::Lua53
                    | SemanticProfile::Lua54
                    | SemanticProfile::Lua55
            ) {
                Value::Integer(value)
            } else {
                Value::Number(value as f64)
            }
        };
        assert_eq!(
            Vm::default().execute_blu_v1(compiled.into_validated_artifact(), BluLimits::default()),
            Ok(vec![
                Value::String(Arc::from(&b"abc[1][2][3]"[..])),
                count(3),
                Value::String(Arc::from(&b"-a-b-c-"[..])),
                count(4),
                Value::String(Arc::from(&b"77aa"[..])),
                count(2)
            ]),
            "{profile}"
        );
    }
}

#[test]
fn string_gsub_expands_substring_and_position_capture_references() {
    let source = make_source(
        b"local a,b=string.gsub('ab12','(%a+)(%d+)','%2-%1-%0') local c,d=string.gsub('ab','a','%1') local e,f=string.gsub('ab','()a','[%1]') return a,b,c,d,e,f"
            .to_vec(),
    );
    for profile in SemanticProfile::ALL {
        let compiled = OwnedCompiler::default()
            .compile(&source, profile, compiler_identity())
            .unwrap();
        let count = |value| {
            if matches!(
                profile,
                SemanticProfile::Blu
                    | SemanticProfile::Lua53
                    | SemanticProfile::Lua54
                    | SemanticProfile::Lua55
            ) {
                Value::Integer(value)
            } else {
                Value::Number(value as f64)
            }
        };
        assert_eq!(
            Vm::default().execute_blu_v1(compiled.into_validated_artifact(), BluLimits::default()),
            Ok(vec![
                Value::String(Arc::from(&b"12-ab-ab12"[..])),
                count(1),
                Value::String(Arc::from(&b"ab"[..])),
                count(1),
                Value::String(Arc::from(&b"[1]b"[..])),
                count(1),
            ]),
            "{profile}"
        );
    }
}

#[test]
fn string_gsub_replacement_escapes_follow_the_lua51_split() {
    let source = make_source(b"return string.gsub('a','a','%q')".to_vec());
    for profile in SemanticProfile::ALL {
        let compiled = OwnedCompiler::default()
            .compile(&source, profile, compiler_identity())
            .unwrap();
        let result =
            Vm::default().execute_blu_v1(compiled.into_validated_artifact(), BluLimits::default());
        if profile == SemanticProfile::Lua51 {
            assert_eq!(
                result,
                Ok(vec![
                    Value::String(Arc::from(&b"q"[..])),
                    Value::Number(1.0)
                ])
            );
        } else {
            assert!(matches!(
                result,
                Err(RuntimeError::UnsupportedLibraryFeature {
                    function: "string.gsub",
                    feature: "nonportable replacement escapes",
                })
            ));
        }
    }
}

#[test]
fn string_gsub_rejects_unimplemented_replacements_structurally() {
    for profile in SemanticProfile::ALL {
        for source in [
            b"return string.gsub('a','a',function() return 'b' end)".as_slice(),
            b"return string.gsub('a','(a)','%2')".as_slice(),
        ] {
            let source = make_source(source.to_vec());
            let compiled = OwnedCompiler::default()
                .compile(&source, profile, compiler_identity())
                .unwrap();
            assert!(matches!(
                Vm::default()
                    .execute_blu_v1(compiled.into_validated_artifact(), BluLimits::default()),
                Err(RuntimeError::UnsupportedLibraryFeature {
                    function: "string.gsub",
                    ..
                }) | Err(RuntimeError::UnsupportedLibraryFeature {
                    function: "string.find",
                    ..
                })
            ));
        }
    }
}

#[test]
fn collectgarbage_collect_preserves_active_roots_and_count_is_numeric() {
    let source = make_source(
        b"local value = { answer = 42 } local result = collectgarbage('collect') local count = collectgarbage('count') return value.answer, result, type(count)"
            .to_vec(),
    );
    for profile in SemanticProfile::ALL {
        let compiled = OwnedCompiler::default()
            .compile(&source, profile, compiler_identity())
            .unwrap();
        let answer = if matches!(
            profile,
            SemanticProfile::Blu
                | SemanticProfile::Lua53
                | SemanticProfile::Lua54
                | SemanticProfile::Lua55
        ) {
            Value::Integer(42)
        } else {
            Value::Number(42.0)
        };
        let collected = match profile {
            SemanticProfile::Luau => Value::Nil,
            SemanticProfile::Lua53 | SemanticProfile::Lua54 | SemanticProfile::Lua55 => {
                Value::Integer(0)
            }
            _ => Value::Number(0.0),
        };
        assert_eq!(
            Vm::default().execute_blu_v1(compiled.into_validated_artifact(), BluLimits::default()),
            Ok(vec![
                answer,
                collected,
                Value::String(Arc::from(&b"number"[..]))
            ]),
            "{profile}"
        );
    }
}

#[test]
fn collectgarbage_rejects_unassigned_commands_and_wrong_types() {
    for profile in SemanticProfile::ALL {
        for (source, expected) in [
            (
                b"return collectgarbage('restart')".as_slice(),
                "unsupported",
            ),
            (b"return collectgarbage({})".as_slice(), "type"),
        ] {
            let source = make_source(source.to_vec());
            let compiled = OwnedCompiler::default()
                .compile(&source, profile, compiler_identity())
                .unwrap();
            let result = Vm::default()
                .execute_blu_v1(compiled.into_validated_artifact(), BluLimits::default());
            match expected {
                "unsupported" => assert!(matches!(
                    result,
                    Err(RuntimeError::UnsupportedLibraryFeature {
                        function: "collectgarbage",
                        ..
                    })
                )),
                "type" => assert!(matches!(result, Err(RuntimeError::Type { .. }))),
                _ => unreachable!(),
            }
        }
    }
}

#[test]
fn table_sort_orders_number_and_string_sequences_without_results() {
    let source = make_source(
        b"local numbers={3,1.5,2} local result=table.sort(numbers,nil) local words={'b','aa','a'} table.sort(words) return numbers[1],numbers[2],numbers[3],words[1],words[2],words[3],result"
            .to_vec(),
    );
    for profile in SemanticProfile::ALL {
        let compiled = OwnedCompiler::default()
            .compile(&source, profile, compiler_identity())
            .unwrap();
        let two = if matches!(
            profile,
            SemanticProfile::Blu
                | SemanticProfile::Lua53
                | SemanticProfile::Lua54
                | SemanticProfile::Lua55
        ) {
            Value::Integer(2)
        } else {
            Value::Number(2.0)
        };
        let three = if matches!(
            profile,
            SemanticProfile::Blu
                | SemanticProfile::Lua53
                | SemanticProfile::Lua54
                | SemanticProfile::Lua55
        ) {
            Value::Integer(3)
        } else {
            Value::Number(3.0)
        };
        assert_eq!(
            Vm::default().execute_blu_v1(compiled.into_validated_artifact(), BluLimits::default()),
            Ok(vec![
                Value::Number(1.5),
                two,
                three,
                Value::String(Arc::from(&b"a"[..])),
                Value::String(Arc::from(&b"aa"[..])),
                Value::String(Arc::from(&b"b"[..])),
                Value::Nil
            ]),
            "{profile}"
        );
    }
}

#[test]
fn table_sort_rejects_custom_comparators_and_unordered_values() {
    for profile in SemanticProfile::ALL {
        for (source, expected) in [
            (
                b"return table.sort({2,1}, function(a,b) return a>b end)".as_slice(),
                "comparator",
            ),
            (b"return table.sort({1,'a'})".as_slice(), "type"),
        ] {
            let source = make_source(source.to_vec());
            let compiled = OwnedCompiler::default()
                .compile(&source, profile, compiler_identity())
                .unwrap();
            let result = Vm::default()
                .execute_blu_v1(compiled.into_validated_artifact(), BluLimits::default());
            match expected {
                "comparator" => assert!(matches!(
                    result,
                    Err(RuntimeError::UnsupportedLibraryFeature {
                        function: "table.sort",
                        ..
                    })
                )),
                "type" => assert!(matches!(result, Err(RuntimeError::Type { .. }))),
                _ => unreachable!(),
            }
        }
    }
}

#[test]
fn table_move_handles_overlap_destinations_and_profile_availability() {
    let source = make_source(
        b"local t={1,2,3,4} local r=table.move(t,1,3,2) local d={} local s=table.move(t,2,4,1,d) local u={1,2,3,4} table.move(u,2,4,1) return r==t,t[1],t[2],t[3],t[4],s==d,d[1],d[2],d[3],u[1],u[2],u[3],u[4]"
            .to_vec(),
    );
    for profile in SemanticProfile::ALL {
        let compiled = OwnedCompiler::default()
            .compile(&source, profile, compiler_identity())
            .unwrap();
        let result =
            Vm::default().execute_blu_v1(compiled.into_validated_artifact(), BluLimits::default());
        if matches!(profile, SemanticProfile::Lua51 | SemanticProfile::Lua52) {
            assert!(matches!(
                result,
                Err(RuntimeError::UnsupportedSemanticProfile {
                    operation: "table.move",
                    ..
                })
            ));
            continue;
        }
        let number = |value| {
            if matches!(
                profile,
                SemanticProfile::Blu
                    | SemanticProfile::Lua53
                    | SemanticProfile::Lua54
                    | SemanticProfile::Lua55
            ) {
                Value::Integer(value)
            } else {
                Value::Number(value as f64)
            }
        };
        assert_eq!(
            result,
            Ok(vec![
                Value::Boolean(true),
                number(1),
                number(1),
                number(2),
                number(3),
                Value::Boolean(true),
                number(1),
                number(2),
                number(3),
                number(2),
                number(3),
                number(4),
                number(4),
            ]),
            "{profile}"
        );
    }
}

#[test]
fn decimal_and_hexadecimal_byte_escapes_decode_by_profile() {
    let decimal = make_source(br#"return "\0\7\65\255""#.to_vec());
    for profile in SemanticProfile::ALL {
        let compiled = OwnedCompiler::default()
            .compile(&decimal, profile, compiler_identity())
            .unwrap();
        assert_eq!(
            compiled.artifact().main().constants,
            [Constant::String(vec![0, 7, 65, 255])],
            "{profile}"
        );
    }

    let hexadecimal = make_source(br#"return "\x00\x41\xff""#.to_vec());
    for profile in SemanticProfile::ALL {
        let result = OwnedCompiler::default().compile(&hexadecimal, profile, compiler_identity());
        if profile == SemanticProfile::Lua51 {
            assert_eq!(
                result.unwrap_err().syntax().unwrap().diagnostics()[0]
                    .code()
                    .as_str(),
                "BLU-LEX-0007"
            );
        } else {
            let compiled = result.unwrap();
            assert_eq!(
                compiled.artifact().main().constants,
                [Constant::String(vec![0, 65, 255])],
                "{profile}"
            );
        }
    }
}

#[test]
fn whitespace_escape_consumes_all_following_ascii_space() {
    let source = make_source(b"return \"left\\z \n\t\r\n right\"".to_vec());
    for profile in SemanticProfile::ALL {
        let result = OwnedCompiler::default().compile(&source, profile, compiler_identity());
        if profile == SemanticProfile::Lua51 {
            assert!(result.is_err());
        } else {
            let compiled = result.unwrap();
            assert_eq!(
                compiled.artifact().main().constants,
                [Constant::String(b"leftright".to_vec())],
                "{profile}"
            );
        }
    }
}

#[test]
fn escaped_physical_line_endings_normalize_to_lf() {
    let source = make_source(b"return \"a\\\nb\", \"c\\\r\nd\", \"e\\\rf\"".to_vec());
    for profile in SemanticProfile::ALL {
        let compiled = OwnedCompiler::default()
            .compile(&source, profile, compiler_identity())
            .unwrap();
        assert_eq!(
            compiled.artifact().main().constants,
            [
                Constant::String(b"a\nb".to_vec()),
                Constant::String(b"c\nd".to_vec()),
                Constant::String(b"e\nf".to_vec()),
            ],
            "{profile}"
        );
    }
}

#[test]
fn unicode_escapes_encode_profile_specific_extended_utf8() {
    let shared = make_source(br#"return "\u{41}\u{D800}\u{1F41B}""#.to_vec());
    let shared_bytes = vec![0x41, 0xed, 0xa0, 0x80, 0xf0, 0x9f, 0x90, 0x9b];
    for profile in SemanticProfile::ALL {
        let result = OwnedCompiler::default().compile(&shared, profile, compiler_identity());
        if matches!(profile, SemanticProfile::Lua51 | SemanticProfile::Lua52) {
            assert!(result.is_err(), "{profile}");
        } else {
            assert_eq!(
                result.unwrap().artifact().main().constants,
                [Constant::String(shared_bytes.clone())],
                "{profile}"
            );
        }
    }

    let extended = make_source(br#"return "\u{110000}\u{7fffffff}""#.to_vec());
    for profile in SemanticProfile::ALL {
        let result = OwnedCompiler::default().compile(&extended, profile, compiler_identity());
        if matches!(
            profile,
            SemanticProfile::Blu | SemanticProfile::Lua54 | SemanticProfile::Lua55
        ) {
            assert_eq!(
                result.unwrap().artifact().main().constants,
                [Constant::String(vec![
                    0xf4, 0x90, 0x80, 0x80, 0xfd, 0xbf, 0xbf, 0xbf, 0xbf, 0xbf,
                ])],
                "{profile}"
            );
        } else {
            assert!(result.is_err(), "{profile}");
        }
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

#[test]
fn owned_noncapturing_functions_lower_to_recursive_prototypes_and_execute() {
    for profile in SemanticProfile::ALL {
        let source = make_source(
            b"local function add(left, right) return left + right end return add(20, 22)".to_vec(),
        );
        let compiled = OwnedCompiler::default()
            .compile(&source, profile, compiler_identity())
            .unwrap();
        let artifact = compiled.artifact();
        assert_eq!(artifact.prototypes().len(), 2, "{profile}");
        assert_eq!(artifact.main().children, [0], "{profile}");
        assert_eq!(artifact.prototypes()[0].parameter_count, 2, "{profile}");
        assert!(
            artifact
                .main()
                .required_features
                .contains(FeatureBits::CLOSURES)
        );
        assert_eq!(
            Vm::default().execute_blu_v1(compiled.into_validated_artifact(), BluLimits::default()),
            Ok(vec![Value::Number(42.0)]),
            "{profile}"
        );
    }
}

#[test]
fn final_calls_adjust_to_local_and_assignment_list_arity() {
    for profile in SemanticProfile::ALL {
        for (bytes, expected) in [
            (
                b"local function pair() return 40, 2 end local a, b, c = pair() return a, b, c"
                    .as_slice(),
                vec![Value::Number(40.0), Value::Number(2.0), Value::Nil],
            ),
            (
                b"local function pair() return 40, 2, 99 end local a, b = 1, pair() return a, b"
                    .as_slice(),
                vec![Value::Number(1.0), Value::Number(40.0)],
            ),
            (
                b"local function pair() return 40, 2 end local a, b = 0, 0 a, b = pair() return a, b"
                    .as_slice(),
                vec![Value::Number(40.0), Value::Number(2.0)],
            ),
            (
                b"local object = {} function object:pair() return 40, 2 end local a, b = object:pair() return a, b"
                    .as_slice(),
                vec![Value::Number(40.0), Value::Number(2.0)],
            ),
        ] {
            let source = make_source(bytes.to_vec());
            let compiled = OwnedCompiler::default()
                .compile(&source, profile, compiler_identity())
                .unwrap();
            assert_eq!(
                Vm::default()
                    .execute_blu_v1(compiled.into_validated_artifact(), BluLimits::default()),
                Ok(expected.clone()),
                "{profile}"
            );
        }
    }
}

#[test]
fn fixed_multi_result_calls_accept_native_results_and_nil_pad() {
    let source = make_source(b"local a, b, c = native_pair() return a, b, c".to_vec());
    let compiled = OwnedCompiler::default()
        .compile(&source, SemanticProfile::Blu, compiler_identity())
        .unwrap();
    let mut vm = Vm::default();
    let function = vm.register_function(|_, _| Ok(vec![Value::Number(40.0), Value::Number(2.0)]));
    vm.set_global(b"native_pair".as_slice(), Value::NativeFunction(function));

    assert_eq!(
        vm.execute_blu_v1(compiled.into_validated_artifact(), BluLimits::default()),
        Ok(vec![Value::Number(40.0), Value::Number(2.0), Value::Nil])
    );
}

#[test]
fn sole_return_calls_forward_all_results_without_growing_blu_callers() {
    for profile in SemanticProfile::ALL {
        let source = make_source(
            b"local function pair(value) if value == 0 then return 40, 2 end return pair(value - 1) end local function forward() return pair(20) end local a, b, c = forward() return a, b, c"
                .to_vec(),
        );
        let compiled = OwnedCompiler::default()
            .compile(&source, profile, compiler_identity())
            .unwrap();
        assert!(
            compiled
                .artifact()
                .prototypes()
                .iter()
                .any(|prototype| prototype
                    .required_features
                    .contains(FeatureBits::RETURN_CALLS)),
            "{profile}"
        );
        assert_eq!(
            Vm::default()
                .with_call_limit(1)
                .execute_blu_v1(compiled.into_validated_artifact(), BluLimits::default(),),
            Ok(vec![
                if matches!(
                    profile,
                    SemanticProfile::Lua53 | SemanticProfile::Lua54 | SemanticProfile::Lua55
                ) {
                    Value::Integer(40)
                } else {
                    Value::Number(40.0)
                },
                if matches!(
                    profile,
                    SemanticProfile::Lua53 | SemanticProfile::Lua54 | SemanticProfile::Lua55
                ) {
                    Value::Integer(2)
                } else {
                    Value::Number(2.0)
                },
                Value::Nil,
            ]),
            "{profile}"
        );

        let source = make_source(
            b"local object = {} function object:pair() return 40, 2 end local function forward() return object:pair() end local a, b = forward() return a, b"
                .to_vec(),
        );
        let compiled = OwnedCompiler::default()
            .compile(&source, profile, compiler_identity())
            .unwrap();
        let integer_profile = matches!(
            profile,
            SemanticProfile::Lua53 | SemanticProfile::Lua54 | SemanticProfile::Lua55
        );
        assert_eq!(
            Vm::default()
                .with_call_limit(1)
                .execute_blu_v1(compiled.into_validated_artifact(), BluLimits::default(),),
            Ok(vec![
                if integer_profile {
                    Value::Integer(40)
                } else {
                    Value::Number(40.0)
                },
                if integer_profile {
                    Value::Integer(2)
                } else {
                    Value::Number(2.0)
                },
            ]),
            "{profile}"
        );
    }

    let source = make_source(b"return native_pair()".to_vec());
    let compiled = OwnedCompiler::default()
        .compile(&source, SemanticProfile::Blu, compiler_identity())
        .unwrap();
    let mut vm = Vm::default();
    let function = vm.register_function(|_, _| Ok(vec![Value::Number(40.0), Value::Number(2.0)]));
    vm.set_global(b"native_pair".as_slice(), Value::NativeFunction(function));
    assert_eq!(
        vm.execute_blu_v1(compiled.into_validated_artifact(), BluLimits::default()),
        Ok(vec![Value::Number(40.0), Value::Number(2.0)])
    );
}

#[test]
fn mixed_return_prefixes_preserve_all_final_call_results() {
    for profile in SemanticProfile::ALL {
        for bytes in [
            b"local function pair() return 2, 3 end local function forward() return 1, pair() end return forward()"
                .as_slice(),
            b"local object = {} function object:pair() return 2, 3 end local function forward() return 1, object:pair() end return forward()"
                .as_slice(),
        ] {
            let source = make_source(bytes.to_vec());
            let compiled = OwnedCompiler::default()
                .compile(&source, profile, compiler_identity())
                .unwrap();
            let integer_profile = matches!(
                profile,
                SemanticProfile::Lua53 | SemanticProfile::Lua54 | SemanticProfile::Lua55
            );
            assert_eq!(
                Vm::default().with_call_limit(1).execute_blu_v1(
                    compiled.into_validated_artifact(),
                    BluLimits::default(),
                ),
                Ok(if integer_profile {
                    vec![Value::Integer(1), Value::Integer(2), Value::Integer(3)]
                } else {
                    vec![
                        Value::Number(1.0),
                        Value::Number(2.0),
                        Value::Number(3.0),
                    ]
                }),
                "{profile}"
            );
        }
    }

    let source = make_source(b"return 1, native_pair()".to_vec());
    let compiled = OwnedCompiler::default()
        .compile(&source, SemanticProfile::Blu, compiler_identity())
        .unwrap();
    let mut vm = Vm::default();
    let function = vm.register_function(|_, _| Ok(vec![Value::Number(2.0), Value::Number(3.0)]));
    vm.set_global(b"native_pair".as_slice(), Value::NativeFunction(function));
    assert_eq!(
        vm.execute_blu_v1(compiled.into_validated_artifact(), BluLimits::default()),
        Ok(vec![
            Value::Number(1.0),
            Value::Number(2.0),
            Value::Number(3.0),
        ])
    );
}

#[test]
fn math_atan_uses_the_explicit_profile_specific_second_argument_contract() {
    for profile in SemanticProfile::ALL {
        let source = make_source(b"return math.atan(1, 0)".to_vec());
        let compiled = OwnedCompiler::default()
            .compile(&source, profile, compiler_identity())
            .unwrap();
        let expected = if matches!(
            profile,
            SemanticProfile::Luau | SemanticProfile::Lua51 | SemanticProfile::Lua52
        ) {
            core::f64::consts::FRAC_PI_4
        } else {
            core::f64::consts::FRAC_PI_2
        };
        assert_eq!(
            Vm::default().execute_blu_v1(compiled.into_validated_artifact(), BluLimits::default(),),
            Ok(vec![Value::Number(expected)]),
            "{profile}"
        );

        let source = make_source(b"return math.atan(1, 'ignored')".to_vec());
        let compiled = OwnedCompiler::default()
            .compile(&source, profile, compiler_identity())
            .unwrap();
        let result =
            Vm::default().execute_blu_v1(compiled.into_validated_artifact(), BluLimits::default());
        if matches!(
            profile,
            SemanticProfile::Luau | SemanticProfile::Lua51 | SemanticProfile::Lua52
        ) {
            assert_eq!(
                result,
                Ok(vec![Value::Number(core::f64::consts::FRAC_PI_4)])
            );
        } else {
            assert!(matches!(
                result,
                Err(blu_runtime::RuntimeError::Type {
                    operation: "math.atan",
                    ..
                })
            ));
        }
    }
}

#[test]
fn math_asin_and_acos_follow_the_shared_profile_contract() {
    for profile in SemanticProfile::ALL {
        let source = make_source(b"return math.asin(1), math.acos(1), math.asin(2)".to_vec());
        let compiled = OwnedCompiler::default()
            .compile(&source, profile, compiler_identity())
            .unwrap();
        let values =
            Vm::default().execute_blu_v1(compiled.into_validated_artifact(), BluLimits::default());
        let values = values.unwrap();
        assert_eq!(values[0], Value::Number(core::f64::consts::FRAC_PI_2));
        assert_eq!(values[1], Value::Number(0.0));
        assert!(matches!(values[2], Value::Number(value) if value.is_nan()));

        let source = make_source(b"return math.acos('x')".to_vec());
        let compiled = OwnedCompiler::default()
            .compile(&source, profile, compiler_identity())
            .unwrap();
        assert!(matches!(
            Vm::default().execute_blu_v1(compiled.into_validated_artifact(), BluLimits::default()),
            Err(blu_runtime::RuntimeError::Type {
                operation: "math.acos",
                ..
            })
        ));
    }
}

#[test]
fn math_floor_and_ceil_return_profile_appropriate_numeric_subtypes() {
    for profile in SemanticProfile::ALL {
        let source =
            make_source(b"return math.floor(1.8), math.ceil(-1.8), math.floor(1e100)".to_vec());
        let compiled = OwnedCompiler::default()
            .compile(&source, profile, compiler_identity())
            .unwrap();
        let modern = matches!(
            profile,
            SemanticProfile::Blu
                | SemanticProfile::Lua53
                | SemanticProfile::Lua54
                | SemanticProfile::Lua55
        );
        assert_eq!(
            Vm::default().execute_blu_v1(compiled.into_validated_artifact(), BluLimits::default(),),
            Ok(vec![
                if modern {
                    Value::Integer(1)
                } else {
                    Value::Number(1.0)
                },
                if modern {
                    Value::Integer(-1)
                } else {
                    Value::Number(-1.0)
                },
                Value::Number(1e100),
            ]),
            "{profile}"
        );
    }
}

#[test]
fn math_modf_preserves_profile_numeric_subtypes_and_two_results() {
    for profile in SemanticProfile::ALL {
        let source = make_source(
            b"local a, b = math.modf(-3.25) local c, d = math.modf(4) return a, b, c, d".to_vec(),
        );
        let compiled = OwnedCompiler::default()
            .compile(&source, profile, compiler_identity())
            .unwrap();
        let modern = matches!(
            profile,
            SemanticProfile::Blu
                | SemanticProfile::Lua53
                | SemanticProfile::Lua54
                | SemanticProfile::Lua55
        );
        assert_eq!(
            Vm::default().execute_blu_v1(compiled.into_validated_artifact(), BluLimits::default()),
            Ok(vec![
                if modern {
                    Value::Integer(-3)
                } else {
                    Value::Number(-3.0)
                },
                Value::Number(-0.25),
                if modern {
                    Value::Integer(4)
                } else {
                    Value::Number(4.0)
                },
                Value::Number(0.0),
            ]),
            "{profile}"
        );
    }
}

#[test]
fn math_modf_rejects_non_numeric_arguments_structurally() {
    let source = make_source(b"return math.modf('x')".to_vec());
    for profile in SemanticProfile::ALL {
        let compiled = OwnedCompiler::default()
            .compile(&source, profile, compiler_identity())
            .unwrap();
        assert!(matches!(
            Vm::default().execute_blu_v1(compiled.into_validated_artifact(), BluLimits::default()),
            Err(RuntimeError::Type {
                operation: "math.modf",
                ..
            })
        ));
    }
}

#[test]
fn math_abs_and_log_follow_profile_numeric_contracts() {
    let source = make_source(b"return math.abs(-3),math.abs(-3.5),math.log(8,2)".to_vec());
    for profile in SemanticProfile::ALL {
        let compiled = OwnedCompiler::default()
            .compile(&source, profile, compiler_identity())
            .unwrap();
        let integral = if matches!(
            profile,
            SemanticProfile::Blu
                | SemanticProfile::Lua53
                | SemanticProfile::Lua54
                | SemanticProfile::Lua55
        ) {
            Value::Integer(3)
        } else {
            Value::Number(3.0)
        };
        let logarithm = if profile == SemanticProfile::Lua51 {
            8.0_f64.ln()
        } else {
            3.0
        };
        assert_eq!(
            Vm::default().execute_blu_v1(compiled.into_validated_artifact(), BluLimits::default()),
            Ok(vec![integral, Value::Number(3.5), Value::Number(logarithm)]),
            "{profile}"
        );
    }
}

#[test]
fn math_min_and_max_preserve_selected_subtypes_and_lua_nan_ordering() {
    let source = make_source(
        b"local nan=0/0 local a=math.min(nan,1) local b=math.max(1,nan) return math.min(3,2),math.max(2,3),math.min(3,2.5),a~=a,b"
            .to_vec(),
    );
    for profile in SemanticProfile::ALL {
        let compiled = OwnedCompiler::default()
            .compile(&source, profile, compiler_identity())
            .unwrap();
        let integral = |value| {
            if matches!(
                profile,
                SemanticProfile::Blu
                    | SemanticProfile::Lua53
                    | SemanticProfile::Lua54
                    | SemanticProfile::Lua55
            ) {
                Value::Integer(value)
            } else {
                Value::Number(value as f64)
            }
        };
        assert_eq!(
            Vm::default().execute_blu_v1(compiled.into_validated_artifact(), BluLimits::default()),
            Ok(vec![
                integral(2),
                integral(3),
                Value::Number(2.5),
                Value::Boolean(true),
                integral(1),
            ]),
            "{profile}"
        );
    }
}

#[test]
fn math_fmod_preserves_modern_integer_semantics_and_zero_split() {
    let source =
        make_source(b"return math.fmod(math.floor(-7),math.floor(3)),math.fmod(-7.5,3)".to_vec());
    let zero_source = make_source(b"return math.fmod(math.floor(7),math.floor(0))".to_vec());
    for profile in SemanticProfile::ALL {
        let compiled = OwnedCompiler::default()
            .compile(&source, profile, compiler_identity())
            .unwrap();
        let modern = matches!(
            profile,
            SemanticProfile::Blu
                | SemanticProfile::Lua53
                | SemanticProfile::Lua54
                | SemanticProfile::Lua55
        );
        assert_eq!(
            Vm::default().execute_blu_v1(compiled.into_validated_artifact(), BluLimits::default()),
            Ok(vec![
                if modern {
                    Value::Integer(-1)
                } else {
                    Value::Number(-1.0)
                },
                Value::Number(-1.5),
            ]),
            "{profile}"
        );

        let compiled = OwnedCompiler::default()
            .compile(&zero_source, profile, compiler_identity())
            .unwrap();
        let result =
            Vm::default().execute_blu_v1(compiled.into_validated_artifact(), BluLimits::default());
        if modern {
            assert_eq!(result, Err(RuntimeError::DivideByZero), "{profile}");
        } else {
            assert!(
                matches!(result, Ok(values) if matches!(values.as_slice(), [Value::Number(value)] if value.is_nan())),
                "{profile}"
            );
        }
    }
}

#[test]
fn owned_named_function_statements_install_recursive_globals() {
    for profile in SemanticProfile::ALL {
        let source = make_source(
            b"function factorial(value) if value <= 1 then return 1 end return value * factorial(value - 1) end return factorial(5)"
                .to_vec(),
        );
        let compiled = OwnedCompiler::default()
            .compile(&source, profile, compiler_identity())
            .unwrap();
        let mut vm = Vm::default();
        assert_eq!(
            vm.execute_blu_v1(compiled.into_validated_artifact(), BluLimits::default()),
            Ok(vec![Value::Number(120.0)]),
            "{profile}"
        );
        assert!(matches!(vm.global(b"factorial"), Some(Value::Closure(_))));

        for bytes in [
            b"local package = { module = {} } function package.module.answer(value) return value + 2 end return package.module.answer(40)"
                .as_slice(),
            b"package = { module = {} } function package.module.answer(value) return value + 2 end return package.module.answer(40)"
                .as_slice(),
        ] {
            let source = make_source(bytes.to_vec());
            let compiled = OwnedCompiler::default()
                .compile(&source, profile, compiler_identity())
                .unwrap();
            assert_eq!(
                Vm::default()
                    .execute_blu_v1(compiled.into_validated_artifact(), BluLimits::default()),
                Ok(vec![Value::Number(42.0)]),
                "{profile}"
            );
        }

        for bytes in [
            b"local object = { base = 40 } function object:add(value) return self.base + value end return object:add(2)"
                .as_slice(),
            b"local object = { base = 40 } function object:make(value) return function(extra) return self.base + value + extra end end local add = object:make(1) return add(1)"
                .as_slice(),
        ] {
            let source = make_source(bytes.to_vec());
            let compiled = OwnedCompiler::default()
                .compile(&source, profile, compiler_identity())
                .unwrap();
            assert_eq!(
                Vm::default()
                    .execute_blu_v1(compiled.into_validated_artifact(), BluLimits::default()),
                Ok(vec![Value::Number(42.0)]),
                "{profile}"
            );
        }
    }
}

#[test]
fn owned_functions_share_mutable_and_transitive_lexical_captures() {
    for (bytes, expected) in [
        (
            b"local value = 1 local function add(delta) value = value + delta return value end return add(2), add(3)"
                .as_slice(),
            vec![Value::Number(3.0), Value::Number(6.0)],
        ),
        (
            b"local value = 40 local function outer() local function inner(extra) return value + extra end return inner end local closure = outer() return closure(2)"
                .as_slice(),
            vec![Value::Number(42.0)],
        ),
        (
            b"local function factorial(value) if value <= 1 then return 1 end return value * factorial(value - 1) end return factorial(5)"
                .as_slice(),
            vec![Value::Number(120.0)],
        ),
        (
            b"local value = 0 local function increment() value = value + 1 end local function read() return value end increment() return read()"
                .as_slice(),
            vec![Value::Number(1.0)],
        ),
        (
            b"local value = 1 local function outer() local value = 42 local function inner() return value end return inner end local closure = outer() return closure()"
                .as_slice(),
            vec![Value::Number(42.0)],
        ),
        (
            b"local make = function(left) return function(right) return left + right end end local add = make(40) return add(2)"
                .as_slice(),
            vec![Value::Number(42.0)],
        ),
    ] {
        for profile in SemanticProfile::ALL {
            let source = make_source(bytes.to_vec());
            let compiled = OwnedCompiler::default()
                .compile(&source, profile, compiler_identity())
                .unwrap();
            assert_eq!(
                Vm::default()
                    .execute_blu_v1(compiled.into_validated_artifact(), BluLimits::default()),
                Ok(expected.clone()),
                "{profile}"
            );
        }
    }
}

#[test]
fn owned_variadic_functions_adjust_fixed_vararg_reads() {
    for bytes in [
        b"local function select(first, ...) local second, third = ... return first, second, third end return select(1, 2, 3, 4)"
            .as_slice(),
        b"local function first(...) return (...) end return first(1, 2, 3)".as_slice(),
    ] {
        let source = make_source(bytes.to_vec());
        for profile in SemanticProfile::ALL {
            let compiled = OwnedCompiler::default()
                .compile(&source, profile, compiler_identity())
                .expect("fixed vararg reads should compile");
            assert!(
                compiled
                    .artifact()
                    .prototypes()
                    .iter()
                    .any(|prototype| prototype.required_features.contains(FeatureBits::VARARGS)),
                "{profile}"
            );
            let modern = matches!(
                profile,
                SemanticProfile::Lua53 | SemanticProfile::Lua54 | SemanticProfile::Lua55
            );
            let expected = if bytes.starts_with(b"local function select") {
                if modern {
                    vec![Value::Integer(1), Value::Integer(2), Value::Integer(3)]
                } else {
                    vec![
                        Value::Number(1.0),
                        Value::Number(2.0),
                        Value::Number(3.0),
                    ]
                }
            } else if modern {
                vec![Value::Integer(1)]
            } else {
                vec![Value::Number(1.0)]
            };
            assert_eq!(
                Vm::default()
                    .execute_blu_v1(compiled.into_validated_artifact(), BluLimits::default()),
                Ok(expected),
                "{profile}"
            );
        }
    }
}

#[test]
fn owned_variadic_functions_forward_dynamic_returns() {
    for (bytes, modern_expected, legacy_expected) in [
        (
            b"local function values(...) return ... end return values(1, 2, 3)".as_slice(),
            vec![Value::Integer(1), Value::Integer(2), Value::Integer(3)],
            vec![Value::Number(1.0), Value::Number(2.0), Value::Number(3.0)],
        ),
        (
            b"local function values(...) return 0, ... end return values(1, 2, 3)".as_slice(),
            vec![
                Value::Integer(0),
                Value::Integer(1),
                Value::Integer(2),
                Value::Integer(3),
            ],
            vec![
                Value::Number(0.0),
                Value::Number(1.0),
                Value::Number(2.0),
                Value::Number(3.0),
            ],
        ),
    ] {
        for profile in SemanticProfile::ALL {
            let source = make_source(bytes.to_vec());
            let compiled = OwnedCompiler::default()
                .compile(&source, profile, compiler_identity())
                .expect("dynamic vararg returns should compile");
            let expected = if matches!(
                profile,
                SemanticProfile::Lua53 | SemanticProfile::Lua54 | SemanticProfile::Lua55
            ) {
                modern_expected.clone()
            } else {
                legacy_expected.clone()
            };
            assert_eq!(
                Vm::default()
                    .execute_blu_v1(compiled.into_validated_artifact(), BluLimits::default()),
                Ok(expected),
                "{profile}"
            );
        }
    }
}

#[test]
fn owned_variadic_functions_forward_dynamic_call_arguments() {
    for (bytes, modern_expected, legacy_expected) in [
        (
            b"local function target(a, b, c) return a, b, c end local function pass(...) return target(0, ...) end return pass(1, 2)"
                .as_slice(),
            vec![Value::Integer(0), Value::Integer(1), Value::Integer(2)],
            vec![Value::Number(0.0), Value::Number(1.0), Value::Number(2.0)],
        ),
        (
            b"local function target(a, b) return a, b end local function pass(...) local first, second = target(...) return first, second end return pass(1, 2)"
                .as_slice(),
            vec![Value::Integer(1), Value::Integer(2)],
            vec![Value::Number(1.0), Value::Number(2.0)],
        ),
        (
            b"local object = {base = 0} function object:target(a, b) return self.base, a, b end local function pass(...) return object:target(...) end return pass(1, 2)"
                .as_slice(),
            vec![Value::Integer(0), Value::Integer(1), Value::Integer(2)],
            vec![Value::Number(0.0), Value::Number(1.0), Value::Number(2.0)],
        ),
    ] {
        for profile in SemanticProfile::ALL {
            let source = make_source(bytes.to_vec());
            let compiled = OwnedCompiler::default()
                .compile(&source, profile, compiler_identity())
                .expect("dynamic vararg call arguments should compile");
            let expected = if matches!(
                profile,
                SemanticProfile::Lua53 | SemanticProfile::Lua54 | SemanticProfile::Lua55
            ) {
                modern_expected.clone()
            } else {
                legacy_expected.clone()
            };
            assert_eq!(
                Vm::default()
                    .execute_blu_v1(compiled.into_validated_artifact(), BluLimits::default()),
                Ok(expected),
                "{profile}"
            );
        }
    }
}

#[test]
fn dynamic_vararg_call_statements_reach_native_functions() {
    let bytes = b"local function pass(...) print(...) end pass(1, 2)";
    for profile in SemanticProfile::ALL {
        let source = make_source(bytes.to_vec());
        let compiled = OwnedCompiler::default()
            .compile(&source, profile, compiler_identity())
            .expect("dynamic vararg native call should compile");
        let mut vm = Vm::default();
        assert_eq!(
            vm.execute_blu_v1(compiled.into_validated_artifact(), BluLimits::default()),
            Ok(Vec::new()),
            "{profile}"
        );
        assert_eq!(vm.take_output(), b"1\t2\n", "{profile}");
    }
}

#[test]
fn owned_variadic_functions_expand_final_table_fields() {
    for (bytes, modern_expected, legacy_expected) in [
        (
            b"local function pack(...) return {0, ...} end local result = pack(1, 2) return result[1], result[2], result[3], #result"
                .as_slice(),
            vec![
                Value::Integer(0),
                Value::Integer(1),
                Value::Integer(2),
                Value::Integer(3),
            ],
            vec![
                Value::Number(0.0),
                Value::Number(1.0),
                Value::Number(2.0),
                Value::Number(3.0),
            ],
        ),
        (
            b"local function pack(...) return {named = 9, ...} end local result = pack(1, 2) return result.named, result[1], result[2]"
                .as_slice(),
            vec![Value::Integer(9), Value::Integer(1), Value::Integer(2)],
            vec![Value::Number(9.0), Value::Number(1.0), Value::Number(2.0)],
        ),
        (
            b"local function pack(...) return {...} end return #pack()".as_slice(),
            vec![Value::Integer(0)],
            vec![Value::Number(0.0)],
        ),
    ] {
        for profile in SemanticProfile::ALL {
            let source = make_source(bytes.to_vec());
            let compiled = OwnedCompiler::default()
                .compile(&source, profile, compiler_identity())
                .expect("dynamic final table varargs should compile");
            let expected = if matches!(
                profile,
                SemanticProfile::Lua53 | SemanticProfile::Lua54 | SemanticProfile::Lua55
            ) {
                modern_expected.clone()
            } else {
                legacy_expected.clone()
            };
            assert_eq!(
                Vm::default()
                    .execute_blu_v1(compiled.into_validated_artifact(), BluLimits::default()),
                Ok(expected),
                "{profile}"
            );
        }
    }
}

#[test]
fn owned_table_constructors_expand_final_call_results_resumably() {
    for (bytes, modern_expected, legacy_expected) in [
        (
            b"local function values() return 1, 2, 3 end local result = {0, values()} return result[1], result[2], result[3], result[4], #result"
                .as_slice(),
            vec![
                Value::Integer(0),
                Value::Integer(1),
                Value::Integer(2),
                Value::Integer(3),
                Value::Integer(4),
            ],
            vec![
                Value::Number(0.0),
                Value::Number(1.0),
                Value::Number(2.0),
                Value::Number(3.0),
                Value::Number(4.0),
            ],
        ),
        (
            b"local object = {} function object:values() return 1, 2 end local result = {object:values()} return result[1], result[2]"
                .as_slice(),
            vec![Value::Integer(1), Value::Integer(2)],
            vec![Value::Number(1.0), Value::Number(2.0)],
        ),
        (
            b"local function values(...) return ... end local function pack(...) return {0, values(...)} end local result = pack(1, 2) return result[1], result[2], result[3]"
                .as_slice(),
            vec![Value::Integer(0), Value::Integer(1), Value::Integer(2)],
            vec![Value::Number(0.0), Value::Number(1.0), Value::Number(2.0)],
        ),
    ] {
        for profile in SemanticProfile::ALL {
            let source = make_source(bytes.to_vec());
            let compiled = OwnedCompiler::default()
                .compile(&source, profile, compiler_identity())
                .expect("final call-result table expansion should compile");
            assert!(compiled.artifact().prototypes().iter().any(|prototype| {
                prototype
                    .required_features
                    .contains(FeatureBits::DYNAMIC_CALL_RESULTS)
            }));
            let expected = if matches!(
                profile,
                SemanticProfile::Lua53 | SemanticProfile::Lua54 | SemanticProfile::Lua55
            ) {
                modern_expected.clone()
            } else {
                legacy_expected.clone()
            };
            assert_eq!(
                Vm::default()
                    .execute_blu_v1(compiled.into_validated_artifact(), BluLimits::default()),
                Ok(expected),
                "{profile}"
            );
        }
    }
}

#[test]
fn owned_table_constructors_expand_native_final_call_results() {
    let source = make_source(
        b"local result = {0, native_values()} return result[1], result[2], result[3]".to_vec(),
    );
    let compiled = OwnedCompiler::default()
        .compile(&source, SemanticProfile::Blu, compiler_identity())
        .unwrap();
    let mut vm = Vm::default();
    let native_values =
        vm.register_function(|_, _| Ok(vec![Value::Number(1.0), Value::Number(2.0)]));
    vm.set_global(
        b"native_values".as_slice(),
        Value::NativeFunction(native_values),
    );
    assert_eq!(
        vm.execute_blu_v1(compiled.into_validated_artifact(), BluLimits::default()),
        Ok(vec![
            Value::Number(0.0),
            Value::Number(1.0),
            Value::Number(2.0),
        ])
    );
}

#[test]
fn owned_table_access_invokes_resumable_index_metamethods() {
    for bytes in [
        b"local fallback = {value = 42} local proxy = setmetatable({}, {__index = fallback}) return proxy.value"
            .as_slice(),
        b"local proxy = setmetatable({}, {__index = function(self, key) return key .. \"!\" end}) return proxy.value"
            .as_slice(),
    ] {
        for profile in SemanticProfile::ALL {
            let source = make_source(bytes.to_vec());
            let compiled = OwnedCompiler::default()
                .compile(&source, profile, compiler_identity())
                .expect("__index source should compile");
            let expected = if bytes.starts_with(b"local fallback") {
                if matches!(
                    profile,
                    SemanticProfile::Lua53 | SemanticProfile::Lua54 | SemanticProfile::Lua55
                ) {
                    Value::Integer(42)
                } else {
                    Value::Number(42.0)
                }
            } else {
                Value::String(b"value!".to_vec().into())
            };
            assert_eq!(
                Vm::default()
                    .execute_blu_v1(compiled.into_validated_artifact(), BluLimits::default()),
                Ok(vec![expected]),
                "{profile}"
            );
        }
    }
}

#[test]
fn owned_table_assignment_invokes_resumable_newindex_metamethods() {
    for bytes in [
        b"local target = {} local proxy = setmetatable({}, {__newindex = target}) proxy.value = 42 return target.value"
            .as_slice(),
        b"local target = {} local proxy = setmetatable({}, {__newindex = function(self, key, value) target[key] = value + 1 end}) proxy.value = 41 return target.value"
            .as_slice(),
    ] {
        for profile in SemanticProfile::ALL {
            let source = make_source(bytes.to_vec());
            let compiled = OwnedCompiler::default()
                .compile(&source, profile, compiler_identity())
                .expect("__newindex source should compile");
            let expected = if matches!(
                profile,
                SemanticProfile::Lua53 | SemanticProfile::Lua54 | SemanticProfile::Lua55
            ) {
                Value::Integer(42)
            } else {
                Value::Number(42.0)
            };
            assert_eq!(
                Vm::default()
                    .execute_blu_v1(compiled.into_validated_artifact(), BluLimits::default()),
                Ok(vec![expected]),
                "{profile}"
            );
        }
    }
}

#[test]
fn owned_table_index_invokes_native_handlers_and_propagates_errors() {
    let source = make_source(
        b"local proxy = setmetatable({}, {__index = native_index}) return proxy.answer".to_vec(),
    );
    let compiled = OwnedCompiler::default()
        .compile(&source, SemanticProfile::Blu, compiler_identity())
        .unwrap();
    let mut vm = Vm::default();
    let native_index = vm.register_function(|_, arguments| {
        Ok(vec![arguments.get(1).cloned().unwrap_or(Value::Nil)])
    });
    vm.set_global(
        b"native_index".as_slice(),
        Value::NativeFunction(native_index),
    );
    assert_eq!(
        vm.execute_blu_v1(compiled.into_validated_artifact(), BluLimits::default()),
        Ok(vec![Value::String(b"answer".to_vec().into())])
    );

    let failing = vm.register_function(|_, _| Err(RuntimeError::Breakpoint { pc: 73 }));
    vm.set_global(b"native_index".as_slice(), Value::NativeFunction(failing));
    let compiled = OwnedCompiler::default()
        .compile(&source, SemanticProfile::Blu, compiler_identity())
        .unwrap();
    assert_eq!(
        vm.execute_blu_v1(compiled.into_validated_artifact(), BluLimits::default()),
        Err(RuntimeError::Breakpoint { pc: 73 })
    );
}

#[test]
fn owned_binary_arithmetic_invokes_resumable_metamethods() {
    let source = make_source(
        b"local mt = {__add = function() return 1 end, __sub = function() return 2 end, __mul = function() return 3 end, __div = function() return 4 end, __mod = function() return 5 end, __pow = function() return 6 end} local left = setmetatable({}, mt) local right = {} return left + right, left - right, left * right, left / right, left % right, left ^ right"
            .to_vec(),
    );
    for profile in SemanticProfile::ALL {
        let compiled = OwnedCompiler::default()
            .compile(&source, profile, compiler_identity())
            .expect("arithmetic metamethod source should compile");
        let expected = if matches!(
            profile,
            SemanticProfile::Lua53 | SemanticProfile::Lua54 | SemanticProfile::Lua55
        ) {
            (1..=6).map(Value::Integer).collect()
        } else {
            (1..=6)
                .map(|value| Value::Number(f64::from(value)))
                .collect()
        };
        assert_eq!(
            Vm::default().execute_blu_v1(compiled.into_validated_artifact(), BluLimits::default()),
            Ok(expected),
            "{profile}"
        );
    }
}

#[test]
fn owned_callable_table_metamethods_resume_across_operator_families() {
    let source = make_source(
        b"local left local right local handler handler = setmetatable({}, {__call = function(self, a, b) return self == handler and a == left and (b == right or b == left or b == nil) end}) local mt = {__add = handler, __unm = handler, __concat = handler, __eq = handler, __lt = handler, __le = handler, __len = handler} left = setmetatable({}, mt) right = setmetatable({}, mt) return left + right, -left, left .. right, left == right, left < right, left <= right, #left"
            .to_vec(),
    );
    for profile in SemanticProfile::ALL {
        let compiled = OwnedCompiler::default()
            .compile(&source, profile, compiler_identity())
            .unwrap();
        let length = if profile == SemanticProfile::Lua51 {
            Value::Number(0.0)
        } else {
            Value::Boolean(true)
        };
        assert_eq!(
            Vm::default().execute_blu_v1(compiled.into_validated_artifact(), BluLimits::default()),
            Ok(vec![
                Value::Boolean(true),
                Value::Boolean(true),
                Value::Boolean(true),
                Value::Boolean(true),
                Value::Boolean(true),
                Value::Boolean(true),
                length,
            ]),
            "{profile}"
        );
    }
}

#[test]
fn owned_unary_negation_invokes_resumable_metamethods() {
    let source = make_source(
        b"local value value = setmetatable({}, {__unm = function(a, b) return a == value and b == value, 99 end}) return -value"
            .to_vec(),
    );
    for profile in SemanticProfile::ALL {
        let compiled = OwnedCompiler::default()
            .compile(&source, profile, compiler_identity())
            .unwrap();
        assert_eq!(
            Vm::default().execute_blu_v1(compiled.into_validated_artifact(), BluLimits::default()),
            Ok(vec![Value::Boolean(true)]),
            "{profile}"
        );
    }
}

#[test]
fn owned_arithmetic_uses_the_right_handler_when_the_left_has_none() {
    let source = make_source(
        b"local left = {} local right right = setmetatable({}, {__add = function(a, b) return a == left and b == right end}) return left + right"
            .to_vec(),
    );
    for profile in SemanticProfile::ALL {
        let compiled = OwnedCompiler::default()
            .compile(&source, profile, compiler_identity())
            .unwrap();
        assert_eq!(
            Vm::default().execute_blu_v1(compiled.into_validated_artifact(), BluLimits::default()),
            Ok(vec![Value::Boolean(true)]),
            "{profile}"
        );
    }
}

#[test]
fn owned_floor_division_invokes_idiv_in_profiles_that_define_it() {
    let source = make_source(
        b"local left = setmetatable({}, {__idiv = function() return 7 end}) return left // {}"
            .to_vec(),
    );
    for profile in [
        SemanticProfile::Luau,
        SemanticProfile::Lua53,
        SemanticProfile::Lua54,
        SemanticProfile::Lua55,
    ] {
        let compiled = OwnedCompiler::default()
            .compile(&source, profile, compiler_identity())
            .unwrap();
        let expected = if matches!(
            profile,
            SemanticProfile::Lua53 | SemanticProfile::Lua54 | SemanticProfile::Lua55
        ) {
            Value::Integer(7)
        } else {
            Value::Number(7.0)
        };
        assert_eq!(
            Vm::default().execute_blu_v1(compiled.into_validated_artifact(), BluLimits::default()),
            Ok(vec![expected]),
            "{profile}"
        );
    }
}

#[test]
fn active_and_suspended_blu_varargs_remain_gc_roots() {
    for bytes in [
        b"local function keep(...) collect() return (...).value end return keep({value = 42})"
            .as_slice(),
        b"local function outer(...) local function inner() collect() end inner() return (...).value end return outer({value = 42})"
            .as_slice(),
    ] {
        let source = make_source(bytes.to_vec());
        let compiled = OwnedCompiler::default()
            .compile(&source, SemanticProfile::Blu, compiler_identity())
            .unwrap();
        let mut vm = Vm::default();
        let collect = vm.register_function(|vm, _| {
            vm.collect(std::iter::empty::<&Value>())?;
            Ok(Vec::new())
        });
        vm.set_global(b"collect".as_slice(), Value::NativeFunction(collect));
        assert_eq!(
            vm.execute_blu_v1(compiled.into_validated_artifact(), BluLimits::default()),
            Ok(vec![Value::Number(42.0)])
        );
    }
}
