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
use blu_runtime::{Dialect, HeapError, MemoryConfig, RuntimeError, Value, Vm};
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
        let expected_constants = if profile == SemanticProfile::Blu {
            [Constant::Integer(40), Constant::Integer(2)]
        } else {
            [Constant::Number(40.0), Constant::Number(2.0)]
        };
        assert_eq!(artifact.main().constants, expected_constants);
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
fn owned_compiler_persists_function_definition_lines_in_bluv2() {
    let source =
        make_source(b"local function answer(value)\n    return value\nend\nreturn answer(42)");
    let compiled = OwnedCompiler::default()
        .compile(&source, SemanticProfile::Lua54, compiler_identity())
        .unwrap();
    assert_eq!(
        compiled.bytes()[4..6],
        blu_bytecode::blu::BLU_V2_VERSION.to_le_bytes()
    );
    let function = compiled
        .artifact()
        .prototypes()
        .iter()
        .find(|prototype| prototype.parameter_count == 1)
        .expect("function prototype");
    assert_eq!(function.line_defined, 1);
    assert_eq!(function.last_line_defined, 3);
    assert_eq!(function.line_info.len(), function.code.len());
    assert!(function.line_info.iter().all(|line| (1..=3).contains(line)));
    assert_eq!(compiled.artifact().main().line_defined, 0);
    assert_eq!(compiled.artifact().main().last_line_defined, 0);
}

#[test]
fn owned_debug_getinfo_exposes_the_level_zero_c_function_for_lua_profiles() {
    let source = make_source(
        b"local info = debug.getinfo(0, 'Snulf')\nreturn info.what == 'C', info.source == '=[C]', info.short_src == '[C]', info.namewhat == 'field', info.name == 'getinfo', info.linedefined == -1, info.lastlinedefined == -1, info.nups == 0, info.nparams == nil, info.isvararg == nil, info.currentline == -1, type(info.func) == 'function'".to_vec(),
    );
    for profile in [
        SemanticProfile::Lua51,
        SemanticProfile::Lua52,
        SemanticProfile::Lua53,
        SemanticProfile::Lua54,
        SemanticProfile::Lua55,
    ] {
        let compiled = OwnedCompiler::default()
            .compile(&source, profile, compiler_identity())
            .unwrap();
        let result = Vm::default()
            .execute_blu_v1(compiled.into_validated_artifact(), BluLimits::default())
            .unwrap();
        assert_eq!(result.len(), 12, "{profile}");
        for (index, value) in result.iter().enumerate() {
            let expected = match index {
                8 | 9 => profile == SemanticProfile::Lua51,
                _ => true,
            };
            assert_eq!(*value, Value::Boolean(expected), "{profile}, field {index}");
        }
    }
}

#[test]
fn owned_debug_getinfo_recovers_direct_global_and_method_call_names() {
    let source = make_source(
        b"function answer() local info = debug.getinfo(1, 'n') return info.namewhat, info.name end\nlocal object = { method = function(self) local info = debug.getinfo(1, 'n') return info.namewhat, info.name end }\nlocal first_what, first_name = answer()\nlocal second_what, second_name = object:method()\nreturn first_what, first_name, second_what, second_name"
            .to_vec(),
    );
    for profile in [
        SemanticProfile::Lua51,
        SemanticProfile::Lua52,
        SemanticProfile::Lua53,
        SemanticProfile::Lua54,
        SemanticProfile::Lua55,
    ] {
        let compiled = OwnedCompiler::default()
            .compile(&source, profile, compiler_identity())
            .unwrap();
        assert_eq!(
            Vm::default().execute_blu_v1(compiled.into_validated_artifact(), BluLimits::default()),
            Ok(vec![
                Value::String(Arc::from(&b"global"[..])),
                Value::String(Arc::from(&b"answer"[..])),
                Value::String(Arc::from(&b"method"[..])),
                Value::String(Arc::from(&b"method"[..])),
            ]),
            "{profile}"
        );
    }
}

#[test]
fn owned_debug_getinfo_isolates_aliased_calls_and_reports_field_calls() {
    let source = make_source(
        b"function answer() local info = debug.getinfo(1, 'n') return info.namewhat, info.name end\nlocal alias = answer\nlocal object = { method = answer }\nlocal key = 'method'\nlocal local_what, local_name = alias()\nlocal field_what, field_name = object.method()\nlocal dynamic_what, dynamic_name = object[key]()\nlocal method_what, method_name = object:method()\nreturn local_what, local_name, field_what, field_name, dynamic_what, dynamic_name, method_what, method_name"
            .to_vec(),
    );
    for profile in [
        SemanticProfile::Lua51,
        SemanticProfile::Lua52,
        SemanticProfile::Lua53,
        SemanticProfile::Lua54,
        SemanticProfile::Lua55,
    ] {
        let compiled = OwnedCompiler::default()
            .compile(&source, profile, compiler_identity())
            .unwrap();
        let result = Vm::default()
            .execute_blu_v1(compiled.into_validated_artifact(), BluLimits::default())
            .unwrap();
        assert_eq!(
            result,
            vec![
                Value::String(Arc::from(&b"local"[..])),
                Value::String(Arc::from(&b"alias"[..])),
                Value::String(Arc::from(&b"field"[..])),
                Value::String(Arc::from(&b"method"[..])),
                Value::String(Arc::from(&b"field"[..])),
                Value::String(Arc::from(&b"?"[..])),
                Value::String(Arc::from(&b"method"[..])),
                Value::String(Arc::from(&b"method"[..])),
            ],
            "{profile}"
        );
    }
}

#[test]
fn owned_debug_getinfo_reports_the_main_chunk_shape_and_function_object() {
    let source = make_source(
        b"local info = debug.getinfo(1, 'Snuf')\nreturn info.what == 'main', info.linedefined == 0, info.lastlinedefined == 0, info.nups, info.nparams, info.isvararg, type(info.func) == 'function'"
            .to_vec(),
    );
    for profile in [
        SemanticProfile::Lua51,
        SemanticProfile::Lua52,
        SemanticProfile::Lua53,
        SemanticProfile::Lua54,
        SemanticProfile::Lua55,
    ] {
        let compiled = OwnedCompiler::default()
            .compile(&source, profile, compiler_identity())
            .unwrap();
        let result =
            Vm::default().execute_blu_v1(compiled.into_validated_artifact(), BluLimits::default());
        let result = result.unwrap();
        assert_eq!(result.len(), 7, "{profile}");
        assert!(
            result[0..3]
                .iter()
                .all(|value| *value == Value::Boolean(true)),
            "{profile}"
        );
        assert_eq!(
            result[3],
            Value::Number(if profile == SemanticProfile::Lua51 {
                0.0
            } else {
                1.0
            }),
            "{profile}"
        );
        assert_eq!(
            result[4],
            if profile == SemanticProfile::Lua51 {
                Value::Nil
            } else {
                Value::Number(0.0)
            },
            "{profile}"
        );
        assert_eq!(
            result[5],
            if profile == SemanticProfile::Lua51 {
                Value::Nil
            } else {
                Value::Boolean(true)
            },
            "{profile}"
        );
        assert_eq!(result[6], Value::Boolean(true), "{profile}");
    }
}

#[test]
fn owned_labels_and_goto_execute_for_blu_and_lua52_plus() {
    let source = make_source(
        b"local value = 0 ::again:: value = value + 1 if value < 3 then goto again end return value"
            .to_vec(),
    );
    for profile in [
        SemanticProfile::Blu,
        SemanticProfile::Lua52,
        SemanticProfile::Lua53,
        SemanticProfile::Lua54,
        SemanticProfile::Lua55,
    ] {
        let compiled = OwnedCompiler::default()
            .compile(&source, profile, compiler_identity())
            .unwrap();
        assert_eq!(
            Vm::default().execute_blu_v1(compiled.into_validated_artifact(), BluLimits::default()),
            Ok(vec![if matches!(
                profile,
                SemanticProfile::Blu
                    | SemanticProfile::Lua53
                    | SemanticProfile::Lua54
                    | SemanticProfile::Lua55
            ) {
                Value::Integer(3)
            } else {
                Value::Number(3.0)
            }]),
            "{profile}"
        );
    }
}

#[test]
fn owned_goto_rejects_missing_duplicate_and_cross_scope_targets() {
    for (source, message) in [
        (b"goto missing return 1".as_slice(), "label 'missing'"),
        (b"::x:: ::x:: return 1".as_slice(), "label 'x'"),
    ] {
        let result = OwnedCompiler::default().compile(
            &make_source(source.to_vec()),
            SemanticProfile::Lua54,
            compiler_identity(),
        );
        match result {
            Err(OwnedCompileError::ControlFlow { message: actual }) => {
                assert!(actual.contains(message), "{actual}");
            }
            Err(OwnedCompileError::Diagnostic(diagnostic)) => {
                assert!(diagnostic.primary().message().contains(message));
            }
            other => panic!("unexpected duplicate-label result: {other:?}"),
        }
    }

    let cross_scope =
        make_source(b"goto inside; do local value = 1 ::inside:: end return 1".to_vec());
    for profile in [
        SemanticProfile::Blu,
        SemanticProfile::Lua52,
        SemanticProfile::Lua53,
        SemanticProfile::Lua54,
        SemanticProfile::Lua55,
    ] {
        assert!(matches!(
            OwnedCompiler::default().compile(&cross_scope, profile, compiler_identity()),
            Err(OwnedCompileError::ControlFlow { message })
                if message.contains("label 'inside'")
        ));
    }

    for profile in [SemanticProfile::Blu, SemanticProfile::Lua55] {
        let global_scope = make_source(b"goto done; global *; ::done:: return 1".to_vec());
        let result = OwnedCompiler::default().compile(&global_scope, profile, compiler_identity());
        assert!(
            matches!(
                &result,
                Err(OwnedCompileError::ControlFlow { message })
                    if message.contains("scope of '*'")
            ),
            "{profile}: {result:?}"
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
fn global_declarations_are_scoped_and_execute_for_blu_and_lua55() {
    for profile in [SemanticProfile::Blu, SemanticProfile::Lua55] {
        let source = make_source(b"global first, second = 1, 2\nreturn first, second".to_vec());
        let compiled = OwnedCompiler::default()
            .compile(&source, profile, compiler_identity())
            .unwrap();
        assert_eq!(
            Vm::default().execute_blu_v1(compiled.into_validated_artifact(), BluLimits::default()),
            Ok(vec![Value::Number(1.0), Value::Number(2.0)]),
            "{profile}"
        );

        let wildcard = make_source(b"global *\nvalue = 7\nreturn value".to_vec());
        let compiled = OwnedCompiler::default()
            .compile(&wildcard, profile, compiler_identity())
            .unwrap();
        assert_eq!(
            Vm::default().execute_blu_v1(compiled.into_validated_artifact(), BluLimits::default()),
            Ok(vec![Value::Number(7.0)]),
            "{profile} wildcard"
        );

        let declaration = make_source(b"global print\nreturn print ~= nil".to_vec());
        let compiled = OwnedCompiler::default()
            .compile(&declaration, profile, compiler_identity())
            .unwrap();
        assert_eq!(
            Vm::default().execute_blu_v1(compiled.into_validated_artifact(), BluLimits::default()),
            Ok(vec![Value::Boolean(true)]),
            "{profile} declaration preserves builtin"
        );
    }
}

#[test]
fn explicit_global_scopes_reject_undeclared_names_and_propagate_into_closures() {
    for profile in [SemanticProfile::Blu, SemanticProfile::Lua55] {
        let rejected = make_source(b"global declared\nreturn missing".to_vec());
        let error = OwnedCompiler::default()
            .compile(&rejected, profile, compiler_identity())
            .unwrap_err();
        assert!(
            matches!(
                error,
                OwnedCompileError::Diagnostic(ref diagnostic)
                    if diagnostic.code().as_str() == "BLU-COMPILE-0012"
            ),
            "{profile}: {error}"
        );

        let closure = make_source(
            b"global value\nvalue = 4\nlocal reader = function() return value end\nreturn reader()"
                .to_vec(),
        );
        let compiled = OwnedCompiler::default()
            .compile(&closure, profile, compiler_identity())
            .unwrap();
        assert_eq!(
            Vm::default().execute_blu_v1(compiled.into_validated_artifact(), BluLimits::default()),
            Ok(vec![Value::Number(4.0)]),
            "{profile} closure"
        );
    }
}

#[test]
fn global_function_declares_a_recursive_global_and_scopes_its_body() {
    for profile in [SemanticProfile::Blu, SemanticProfile::Lua55] {
        let source =
            make_source(b"global function answer() return 42 end\nreturn answer()".to_vec());
        let compiled = OwnedCompiler::default()
            .compile(&source, profile, compiler_identity())
            .unwrap();
        assert_eq!(
            Vm::default().execute_blu_v1(compiled.into_validated_artifact(), BluLimits::default()),
            Ok(vec![Value::Number(42.0)]),
            "{profile}"
        );

        let rejected = make_source(b"global function answer() return missing end".to_vec());
        let error = OwnedCompiler::default()
            .compile(&rejected, profile, compiler_identity())
            .unwrap_err();
        assert!(
            matches!(
                error,
                OwnedCompileError::Diagnostic(ref diagnostic)
                    if diagnostic.code().as_str() == "BLU-COMPILE-0012"
            ),
            "{profile}: {error}"
        );
    }
}

#[test]
fn named_vararg_tables_materialize_values_and_n_for_blu_and_lua55() {
    for profile in [SemanticProfile::Blu, SemanticProfile::Lua55] {
        let source = make_source(
            b"local function collect(... args) return args.n, args[1], args[2] end\nreturn collect(3, 4)"
                .to_vec(),
        );
        let compiled = OwnedCompiler::default()
            .compile(&source, profile, compiler_identity())
            .unwrap();
        assert_eq!(
            Vm::default().execute_blu_v1(compiled.into_validated_artifact(), BluLimits::default()),
            Ok(vec![
                Value::Integer(2),
                Value::Integer(3),
                Value::Integer(4)
            ]),
            "{profile}"
        );

        let rejected = make_source(b"local function collect(... args) args = {} end".to_vec());
        let error = OwnedCompiler::default()
            .compile(&rejected, profile, compiler_identity())
            .unwrap_err();
        assert!(
            matches!(
                error,
                OwnedCompileError::Diagnostic(ref diagnostic)
                    if diagnostic.code().as_str() == "BLU-COMPILE-0011"
            ),
            "{profile}: {error}"
        );
    }
}

#[test]
fn uninitialized_local_lowers_to_nil_for_every_profile() {
    for profile in [SemanticProfile::Blu, SemanticProfile::Lua51] {
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
            SemanticProfile::Blu
                | SemanticProfile::Lua53
                | SemanticProfile::Lua54
                | SemanticProfile::Lua55
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
        [Constant::Integer(1), Constant::Integer(2)]
    );
    assert_eq!(
        Vm::new(Dialect::Blu)
            .execute_blu_v1(compiled.into_validated_artifact(), BluLimits::default()),
        Ok(vec![Value::Integer(1)])
    );

    let source = make_source(b"local value\nvalue, value = 1, 2\nreturn value".to_vec());
    let compiled = OwnedCompiler::default()
        .compile(&source, SemanticProfile::Blu, compiler_identity())
        .unwrap();
    assert_eq!(
        Vm::new(Dialect::Blu)
            .execute_blu_v1(compiled.into_validated_artifact(), BluLimits::default()),
        Ok(vec![Value::Integer(1)])
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
            SemanticProfile::Blu
                | SemanticProfile::Lua53
                | SemanticProfile::Lua54
                | SemanticProfile::Lua55
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
        Ok(vec![Value::Integer(9), Value::Nil])
    );

    let source = make_source(b"local kept\nkept = 1, 2\nreturn kept".to_vec());
    let compiled = OwnedCompiler::default()
        .compile(&source, SemanticProfile::Blu, compiler_identity())
        .unwrap();
    assert_eq!(
        compiled.artifact().main().constants,
        [Constant::Nil, Constant::Integer(1), Constant::Integer(2)]
    );
    assert_eq!(
        Vm::new(Dialect::Blu)
            .execute_blu_v1(compiled.into_validated_artifact(), BluLimits::default()),
        Ok(vec![Value::Integer(1)])
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
        SemanticProfile::Blu,
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
        let expected_code = if matches!(
            profile,
            SemanticProfile::Lua53 | SemanticProfile::Lua54 | SemanticProfile::Lua55
        ) {
            vec![
                Instruction::NewTable { destination: 0 },
                Instruction::LoadConstant {
                    destination: 1,
                    constant: 0,
                },
                Instruction::LoadConstant {
                    destination: 2,
                    constant: 1,
                },
                Instruction::FloorDivide {
                    destination: 3,
                    left: 1,
                    right: 2,
                },
                Instruction::Return { first: 3, count: 1 },
            ]
        } else {
            vec![
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
        };
        assert_eq!(compiled.artifact().main().code, expected_code);
        assert_eq!(
            compiled.artifact().main().source_map[if expected_code.len() == 5 { 3 } else { 2 }],
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
fn blu_floor_division_uses_modern_integer_and_float_semantics() {
    let source = make_source(
        b"return math.floor(7)//math.floor(3),7.0//3,-math.floor(7)//math.floor(3),1.0//0.0"
            .to_vec(),
    );
    let compiled = OwnedCompiler::default()
        .compile(&source, SemanticProfile::Blu, compiler_identity())
        .unwrap();
    let values =
        Vm::default().execute_blu_v1(compiled.into_validated_artifact(), BluLimits::default());
    assert_eq!(
        values,
        Ok(vec![
            Value::Integer(2),
            Value::Number(2.0),
            Value::Integer(-3),
            Value::Number(f64::INFINITY),
        ])
    );

    let zero = make_source(b"return math.floor(1)//math.floor(0)".to_vec());
    let compiled = OwnedCompiler::default()
        .compile(&zero, SemanticProfile::Blu, compiler_identity())
        .unwrap();
    assert_eq!(
        Vm::default().execute_blu_v1(compiled.into_validated_artifact(), BluLimits::default()),
        Err(RuntimeError::DivideByZero)
    );
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

    let unresolved = make_source(b"global answer\nother = 1".to_vec());
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
            SemanticProfile::Blu
                | SemanticProfile::Lua53
                | SemanticProfile::Lua54
                | SemanticProfile::Lua55
        );
        if integer_profile {
            assert_eq!(
                compiled.artifact().main().constants,
                [Constant::Integer(40), Constant::Integer(2)]
            );
            assert_eq!(
                compiled.artifact().main().required_features,
                FeatureBits::BASELINE
                    | FeatureBits::INTEGER_CONSTANTS
                    | if matches!(
                        profile,
                        SemanticProfile::Lua53 | SemanticProfile::Lua54 | SemanticProfile::Lua55
                    ) {
                        FeatureBits::IMPLICIT_ENVIRONMENT
                    } else {
                        FeatureBits::empty()
                    }
            );
        } else {
            assert_eq!(
                compiled.artifact().main().constants,
                [Constant::Number(40.0), Constant::Number(2.0)]
            );
            assert_eq!(
                compiled.artifact().main().required_features,
                FeatureBits::BASELINE
                    | if profile == SemanticProfile::Lua52 {
                        FeatureBits::IMPLICIT_ENVIRONMENT
                    } else {
                        FeatureBits::empty()
                    }
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
fn indexed_assignment_commits_snapshotted_targets_right_to_left() {
    let source = make_source(
        b"local values = {1} local alias = values values[1], alias[1] = 5, 6 return values[1]"
            .to_vec(),
    );
    for profile in SemanticProfile::ALL {
        let compiled = OwnedCompiler::default()
            .compile(&source, profile, compiler_identity())
            .unwrap();
        let expected = if matches!(
            profile,
            SemanticProfile::Blu
                | SemanticProfile::Lua53
                | SemanticProfile::Lua54
                | SemanticProfile::Lua55
        ) {
            Value::Integer(5)
        } else {
            Value::Number(5.0)
        };
        assert_eq!(
            Vm::default().execute_blu_v1(compiled.into_validated_artifact(), BluLimits::default()),
            Ok(vec![expected]),
            "{profile}"
        );
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
    let escaped = OwnedCompiler::default()
        .compile(&escaped, SemanticProfile::Blu, compiler_identity())
        .unwrap();
    assert_eq!(
        Vm::default().execute_blu_v1(escaped.into_validated_artifact(), BluLimits::default()),
        Ok(vec![Value::Nil])
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
fn repeat_until_keeps_body_locals_visible_to_its_condition() {
    let source = make_source(
        b"local count = 0\nrepeat\nlocal marker = count + 1\ncount = marker\nuntil marker == 3\nreturn count == 3"
            .to_vec(),
    );
    for profile in SemanticProfile::ALL {
        let compiled = OwnedCompiler::default()
            .compile(&source, profile, compiler_identity())
            .expect("repeat condition should see body locals");
        assert_eq!(
            Vm::default().execute_blu_v1(compiled.into_validated_artifact(), BluLimits::default()),
            Ok(vec![Value::Boolean(true)]),
            "{profile}"
        );
    }
}

#[test]
fn lua54_repeat_until_closes_body_locals_after_testing_condition() {
    let source = make_source(
        b"local events = ''\nlocal metatable = {__close = function() events = events .. 'close' end}\nrepeat\nlocal value <close> = setmetatable({}, metatable)\nuntil events == 'close'\nreturn events == 'closeclose'"
            .to_vec(),
    );
    for profile in [SemanticProfile::Lua54, SemanticProfile::Lua55] {
        let compiled = OwnedCompiler::default()
            .compile(&source, profile, compiler_identity())
            .expect("repeat to-be-closed source should compile");
        assert_eq!(
            Vm::default().execute_blu_v1(compiled.into_validated_artifact(), BluLimits::default()),
            Ok(vec![Value::Boolean(true)]),
            "{profile}"
        );
    }
}

#[test]
fn blu_and_luau_if_expressions_short_circuit_and_select_values() {
    let source = make_source(
        b"return if true then 10 else error('unreachable'),if false then error('unreachable') else 20,if false then 1 elseif true then 30 else 3"
            .to_vec(),
    );
    for profile in SemanticProfile::ALL {
        let compiled = OwnedCompiler::default().compile(&source, profile, compiler_identity());
        if matches!(profile, SemanticProfile::Blu | SemanticProfile::Luau) {
            let compiled = compiled.unwrap();
            let number = |value| {
                if profile == SemanticProfile::Blu {
                    Value::Integer(value)
                } else {
                    Value::Number(value as f64)
                }
            };
            assert_eq!(
                Vm::default()
                    .execute_blu_v1(compiled.into_validated_artifact(), BluLimits::default()),
                Ok(vec![number(10), number(20), number(30)]),
                "{profile}"
            );
        } else {
            assert!(matches!(compiled, Err(OwnedCompileError::Syntax(_))));
        }
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
        let compiled = OwnedCompiler::default()
            .compile(&source, profile, compiler_identity())
            .expect("zero-step semantics should be checked at runtime");
        assert!(matches!(
            Vm::default().execute_blu_v1(compiled.into_validated_artifact(), BluLimits::default()),
            Err(RuntimeError::Raised(_))
        ));
    }

    let dynamic = make_source(
        b"local step = 1 local total = 0\nfor index = 1, 3, step do total = total + index end return total"
            .to_vec(),
    );
    let compiled = OwnedCompiler::default()
        .compile(&dynamic, SemanticProfile::Blu, compiler_identity())
        .expect("dynamic Blu step should be checked at runtime");
    assert_eq!(
        Vm::default().execute_blu_v1(compiled.into_validated_artifact(), BluLimits::default()),
        Ok(vec![Value::Integer(6)])
    );
}

#[test]
fn numeric_for_stops_when_integer_iteration_wraps() {
    let source = make_source(
        b"local up = 0 for index = math.mininteger, math.maxinteger, math.maxinteger do up = up + 1 end local down = 0 for index = math.maxinteger, math.mininteger, math.mininteger do down = down + 1 end return up, down".to_vec(),
    );
    for profile in [
        SemanticProfile::Lua53,
        SemanticProfile::Lua54,
        SemanticProfile::Lua55,
    ] {
        let compiled = OwnedCompiler::default()
            .compile(&source, profile, compiler_identity())
            .unwrap();
        assert_eq!(
            Vm::default().execute_blu_v1(compiled.into_validated_artifact(), BluLimits::default()),
            Ok(vec![Value::Integer(3), Value::Integer(2)]),
            "{profile}"
        );
    }
}

#[test]
fn numeric_for_snapshots_dynamic_steps_for_profiles_with_assigned_zero_behavior() {
    let dynamic = make_source(
        b"local calls = 0 local function getstep() calls = calls + 1 return -2 end local total = 0 for index = 5, 1, getstep() do total = total + index end return total, calls"
            .to_vec(),
    );
    let zero = make_source(
        b"local step = 0 local count = 0 for index = 1, 1, step do count = count + 1 if count == 1 then break end end return count"
            .to_vec(),
    );
    for profile in SemanticProfile::ALL {
        let compiled = OwnedCompiler::default()
            .compile(&dynamic, profile, compiler_identity())
            .expect("profile assigns dynamic step direction");
        let expected = if matches!(profile, SemanticProfile::Blu | SemanticProfile::Lua53) {
            vec![Value::Integer(9), Value::Integer(1)]
        } else {
            vec![Value::Number(9.0), Value::Number(1.0)]
        };
        assert_eq!(
            Vm::default().execute_blu_v1(compiled.into_validated_artifact(), BluLimits::default()),
            Ok(expected),
            "{profile}"
        );
    }

    for profile in [
        SemanticProfile::Luau,
        SemanticProfile::Lua51,
        SemanticProfile::Lua52,
        SemanticProfile::Lua53,
    ] {
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
        let compiled = OwnedCompiler::default()
            .compile(&zero, profile, compiler_identity())
            .expect("dynamic zero behavior should be checked at runtime");
        assert!(
            matches!(
                Vm::default()
                    .execute_blu_v1(compiled.into_validated_artifact(), BluLimits::default()),
                Err(RuntimeError::Raised(_))
            ),
            "{profile}"
        );
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
            let compiled = OwnedCompiler::default()
                .compile(&source, profile, compiler_identity())
                .expect("generic for should compile");
            let result = if profile == SemanticProfile::Lua53 {
                Value::Integer(result)
            } else {
                Value::Number(result as f64)
            };
            assert_eq!(
                Vm::default().execute_blu_v1(
                    compiled.into_validated_artifact(),
                    BluLimits::default()
                ),
                Ok(vec![result]),
                "{profile}"
            );
        }
    }

    let metamethod_source = make_source(
        b"local mt={} function mt.__band(a,b) return 11 end function mt.__bor(a,b) return 12 end function mt.__bxor(a,b) return 13 end function mt.__shl(a,b) return 14 end function mt.__shr(a,b) return 15 end function mt.__bnot(a,b) return rawequal(a,b) and 16 or 0 end local x=setmetatable({},mt) return x&1,1|x,x~1,1<<x,x>>1,~x"
            .to_vec(),
    );
    for profile in [
        SemanticProfile::Blu,
        SemanticProfile::Lua53,
        SemanticProfile::Lua54,
        SemanticProfile::Lua55,
    ] {
        let compiled = OwnedCompiler::default()
            .compile(&metamethod_source, profile, compiler_identity())
            .unwrap();
        assert_eq!(
            Vm::default().execute_blu_v1(compiled.into_validated_artifact(), BluLimits::default()),
            Ok(vec![
                Value::Integer(11),
                Value::Integer(12),
                Value::Integer(13),
                Value::Integer(14),
                Value::Integer(15),
                Value::Integer(16),
            ]),
            "{profile}"
        );
    }
}

#[test]
fn direct_table_iteration_uses_profile_owned_iter_hook() {
    let source = make_source(
        b"local values=setmetatable({}, {__iter=function(value) local function iterator(_,index) if index<2 then return index+1,'hook'..tostring(index+1) end end return iterator,value,0 end}) local result='' for index,value in values do result=result..value end return result"
            .to_vec(),
    );
    for profile in [SemanticProfile::Blu, SemanticProfile::Luau] {
        let compiled = OwnedCompiler::default()
            .compile(&source, profile, compiler_identity())
            .unwrap();
        assert_eq!(
            Vm::default().execute_blu_v1(compiled.into_validated_artifact(), BluLimits::default()),
            Ok(vec![Value::String(Arc::from(&b"hook1hook2"[..]))]),
            "{profile}"
        );
    }
}

#[test]
fn luau_table_hash_fixture_order_is_observable() {
    let source = make_source(
        b"local t={['Mountains']=true,['Canyons']=true,['Dunes']=true,['Arctic']=true,['Lavaflow']=true,['Hills']=true,['Plains']=true,['Marsh']=true,['Water']=true} local result='' for key in pairs(t) do result=result..key end return result".to_vec(),
    );
    let compiled = OwnedCompiler::default()
        .compile(&source, SemanticProfile::Luau, compiler_identity())
        .unwrap();
    let result = Vm::default()
        .execute_blu_v1(compiled.into_validated_artifact(), BluLimits::default())
        .unwrap();
    assert_eq!(
        result,
        vec![Value::String(Arc::from(
            &b"ArcticDunesCanyonsWaterMountainsHillsLavaflowPlainsMarsh"[..]
        ))]
    );
}

#[test]
fn luau_userdata_namecall_uses_the_metatable_handler() {
    let source = make_source(
        b"local object = newproxy(true) getmetatable(object).__namecall = function(self, argument) return 42 + argument end return object:Foo(10)".to_vec(),
    );
    let compiled = OwnedCompiler::default()
        .compile(&source, SemanticProfile::Luau, compiler_identity())
        .unwrap();
    assert_eq!(
        Vm::default().execute_blu_v1(compiled.into_validated_artifact(), BluLimits::default()),
        Ok(vec![Value::Number(52.0)])
    );
}

#[test]
fn pairs_honors_profile_available_pairs_metamethods_and_resumes_handlers() {
    let source = make_source(
        b"local wrapped=coroutine.wrap(function() local marker={} local mt={__pairs=function(value) local resume=coroutine.yield(value) return next,value,resume end} return pairs(setmetatable(marker,mt)) end) local yielded=wrapped() local iterator,state,control=wrapped('done') return yielded==state,type(iterator),control".to_vec(),
    );
    for profile in [
        SemanticProfile::Blu,
        SemanticProfile::Luau,
        SemanticProfile::Lua52,
        SemanticProfile::Lua53,
        SemanticProfile::Lua54,
        SemanticProfile::Lua55,
    ] {
        let compiled = OwnedCompiler::default()
            .compile(&source, profile, compiler_identity())
            .unwrap();
        assert_eq!(
            Vm::default().execute_blu_v1(compiled.into_validated_artifact(), BluLimits::default()),
            Ok(vec![
                Value::Boolean(true),
                Value::String(Arc::from(&b"function"[..])),
                Value::String(Arc::from(&b"done"[..])),
            ]),
            "{profile}"
        );
    }
}

#[test]
fn ipairs_honors_profile_available_ipairs_metamethods_and_resumes_handlers() {
    let source = make_source(
        b"local wrapped=coroutine.wrap(function() local marker={} local mt={__ipairs=function(value) local resume=coroutine.yield(value) local function iterator(_,index) if index<1 then return index+1,resume end end return iterator,value,0 end} return ipairs(setmetatable(marker,mt)) end) local yielded=wrapped() local iterator,state,control=wrapped('done') local first,value=iterator(state,control) return yielded==state and first==1 and value=='done'".to_vec(),
    );
    for profile in [SemanticProfile::Lua52, SemanticProfile::Lua53] {
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
fn ipairs_reads_values_through_a_table_index_metamethod() {
    let source = make_source(
        b"local value = {n = 10} setmetatable(value, {__index = function(table, key) if key <= table.n then return key * 10 end end}) local count = 0 for key, item in ipairs(value) do count = count + 1 assert(key == count and item == count * 10) end return count == value.n".to_vec(),
    );
    for profile in [SemanticProfile::Lua54, SemanticProfile::Lua55] {
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
fn table_library_operations_honor_index_and_newindex_metamethods() {
    let source = make_source(
        b"local function test(proxy, target) for index = 1, 10 do table.insert(proxy, 1, index) end table.sort(proxy) assert(table.concat(proxy, ',') == '1,2,3,4,5,6,7,8,9,10') for index = 1, 8 do assert(table.remove(proxy, 1) == index) end local first, second, third = table.unpack(proxy) return #proxy == 2 and #target == 2 and first == 9 and second == 10 and third == nil end local target = {} local proxy = setmetatable({}, {__len = function() return #target end, __index = target, __newindex = target}) return test(proxy, target)".to_vec(),
    );
    for profile in [SemanticProfile::Lua54, SemanticProfile::Lua55] {
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
fn ipairs_wraps_integer_index_overflow_like_lua() {
    let source = make_source(
        b"local iterator = ipairs({}) local key, value = iterator({[math.mininteger] = 10}, math.maxinteger) local next_key, next_value = iterator({[math.mininteger] = 10}, key) return key, value, next_key, next_value".to_vec(),
    );
    for profile in [
        SemanticProfile::Lua53,
        SemanticProfile::Lua54,
        SemanticProfile::Lua55,
    ] {
        let compiled = OwnedCompiler::default()
            .compile(&source, profile, compiler_identity())
            .unwrap();
        assert_eq!(
            Vm::default().execute_blu_v1(compiled.into_validated_artifact(), BluLimits::default()),
            Ok(vec![
                Value::Integer(i64::MIN),
                Value::Integer(10),
                Value::Nil,
                Value::Nil,
            ]),
            "{profile}"
        );
    }
}

#[test]
fn table_insert_wraps_a_metamethod_length_overflow_like_lua() {
    let source = make_source(
        b"local value = setmetatable({}, {__len = function() return math.maxinteger end}) table.insert(value, 20) local key, item = next(value) return key == math.mininteger, item".to_vec(),
    );
    for profile in [SemanticProfile::Lua54, SemanticProfile::Lua55] {
        let compiled = OwnedCompiler::default()
            .compile(&source, profile, compiler_identity())
            .unwrap();
        assert_eq!(
            Vm::default().execute_blu_v1(compiled.into_validated_artifact(), BluLimits::default()),
            Ok(vec![Value::Boolean(true), Value::Integer(20)]),
            "{profile}"
        );
    }
}

#[test]
fn generic_for_adjusts_the_final_control_call_exactly_once() {
    let source = make_source(
        b"local calls = 0 local function iterator(limit, control) local value = control + 1 if value <= limit then return value, value end end local function controls() calls = calls + 1 return iterator, 3, 0 end local total = 0 for key, value in controls() do total = total + value end return total, calls"
            .to_vec(),
    );
    for profile in SemanticProfile::ALL {
        let compiled = OwnedCompiler::default()
            .compile(&source, profile, compiler_identity())
            .expect("generic for should compile");
        let integer_profile = profile == SemanticProfile::Lua53;
        assert_eq!(
            Vm::default().execute_blu_v1(compiled.into_validated_artifact(), BluLimits::default()),
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
fn lua52_plus_lexical_environment_overrides_global_identifier_access() {
    let source = make_source(
        b"local _ENV={answer=40} answer=answer+2 local function read() answer=answer+1 return answer end read() return answer".to_vec(),
    );
    for profile in [
        SemanticProfile::Lua52,
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
            Value::Integer(43)
        } else {
            Value::Number(43.0)
        };
        let mut vm = Vm::default();
        assert_eq!(
            vm.execute_blu_v1(compiled.into_validated_artifact(), BluLimits::default()),
            Ok(vec![expected.clone()]),
            "{profile}"
        );
        assert_eq!(
            vm.global(b"answer"),
            None,
            "lexical environment leaked globally: {profile}"
        );
    }
}

#[test]
fn lua52_plus_default_environment_supports_nested_global_closure_reads_and_writes() {
    let source = make_source(
        b"answer = 40 local function make() local function read() answer = answer + 2 return answer end return read end local read = make() read() return answer, _ENV.answer"
            .to_vec(),
    );
    for profile in [
        SemanticProfile::Lua52,
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
            Value::Integer(42)
        } else {
            Value::Number(42.0)
        };
        let mut vm = Vm::default();
        assert_eq!(
            vm.execute_blu_v1(compiled.into_validated_artifact(), BluLimits::default()),
            Ok(vec![expected.clone(), expected.clone()]),
            "{profile}"
        );
        assert_eq!(vm.global(b"answer"), Some(&expected), "{profile}");
    }
}

#[test]
fn lua52_plus_lexical_environment_handles_lists_and_global_function_syntax() {
    let source = make_source(
        b"local _ENV={a=1,b=2} a,b=b,a function answer() return a+b end return answer(),a,b"
            .to_vec(),
    );
    for profile in [
        SemanticProfile::Lua52,
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
            vec![Value::Integer(3), Value::Integer(2), Value::Integer(1)]
        } else {
            vec![Value::Number(3.0), Value::Number(2.0), Value::Number(1.0)]
        };
        assert_eq!(
            Vm::default().execute_blu_v1(compiled.into_validated_artifact(), BluLimits::default()),
            Ok(expected),
            "{profile}"
        );
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
fn lua54_generic_for_closes_its_fourth_control_on_exit_and_break() {
    let source = make_source(
        b"local closed=0 local resource=setmetatable({}, {__close=function() closed=closed+1 end}) local function iterator(state,control) local value=control+1 if value<=2 then return value,value end end local total=0 for value in iterator,nil,0,resource do total=total+value end local second=setmetatable({}, {__close=function() closed=closed+1 end}) for value in iterator,nil,0,second do break end return total,closed".to_vec(),
    );
    for profile in [SemanticProfile::Lua54, SemanticProfile::Lua55] {
        let compiled = OwnedCompiler::default()
            .compile(&source, profile, compiler_identity())
            .unwrap();
        assert_eq!(
            Vm::default().execute_blu_v1(compiled.into_validated_artifact(), BluLimits::default()),
            Ok(vec![Value::Integer(3), Value::Integer(2)]),
            "{profile}"
        );
    }
}

#[test]
fn lua54_to_be_closed_locals_close_on_normal_scope_exit() {
    let source = make_source(
        b"local closed=0 do local <close> resource=setmetatable({}, {__close=function() closed=closed+1 end}) end return closed".to_vec(),
    );
    for profile in [SemanticProfile::Lua54, SemanticProfile::Lua55] {
        let compiled = OwnedCompiler::default()
            .compile(&source, profile, compiler_identity())
            .unwrap();
        assert_eq!(
            Vm::default().execute_blu_v1(compiled.into_validated_artifact(), BluLimits::default()),
            Ok(vec![Value::Integer(1)]),
            "{profile}"
        );
    }
}

#[test]
fn lua54_to_be_closed_locals_close_on_break_and_return() {
    let break_source = make_source(
        b"local closed=0 while true do local <close> resource=setmetatable({}, {__close=function() closed=closed+1 end}) break end return closed".to_vec(),
    );
    let return_source = make_source(
        b"local closed=0 local <close> resource=setmetatable({}, {__close=function() closed=closed+1 end}) return closed".to_vec(),
    );
    for profile in [SemanticProfile::Lua54, SemanticProfile::Lua55] {
        let compiled = OwnedCompiler::default()
            .compile(&break_source, profile, compiler_identity())
            .unwrap();
        assert_eq!(
            Vm::default().execute_blu_v1(compiled.into_validated_artifact(), BluLimits::default()),
            Ok(vec![Value::Integer(1)]),
            "break: {profile}"
        );

        let compiled = OwnedCompiler::default()
            .compile(&return_source, profile, compiler_identity())
            .unwrap();
        assert_eq!(
            Vm::default().execute_blu_v1(compiled.into_validated_artifact(), BluLimits::default()),
            Ok(vec![Value::Integer(0)]),
            "return evaluates before close: {profile}"
        );
    }
}

#[test]
fn lua54_const_locals_reject_assignment_and_close_values_require_handlers() {
    let const_source = make_source(b"local <const> value=1 value=2".to_vec());
    let close_const_source = make_source(b"local <close> value=nil value=2".to_vec());
    for profile in [SemanticProfile::Lua54, SemanticProfile::Lua55] {
        let error = OwnedCompiler::default()
            .compile(&const_source, profile, compiler_identity())
            .expect_err("const assignment must be rejected");
        assert!(
            error.to_string().contains("const variable 'value'"),
            "{profile}: {error}"
        );

        let error = OwnedCompiler::default()
            .compile(&close_const_source, profile, compiler_identity())
            .expect_err("to-be-closed assignment must be rejected");
        assert!(
            error.to_string().contains("const variable 'value'"),
            "{profile}: {error}"
        );
    }

    let invalid_close_source =
        make_source(b"local <close> resource=setmetatable({}, {}) return 1".to_vec());
    for profile in [SemanticProfile::Lua54, SemanticProfile::Lua55] {
        let compiled = OwnedCompiler::default()
            .compile(&invalid_close_source, profile, compiler_identity())
            .unwrap();
        assert!(
            matches!(
                Vm::default()
                    .execute_blu_v1(compiled.into_validated_artifact(), BluLimits::default()),
                Err(RuntimeError::Type {
                    operation: "close",
                    ..
                })
            ),
            "{profile}"
        );
    }
}

#[test]
fn lua55_global_const_visibility_survives_nested_function_boundaries() {
    let source = make_source(
        b"global a, var1<const>, z; local function foo() a=20; z=function() var1=12 end end"
            .to_vec(),
    );
    let error = OwnedCompiler::default()
        .compile(&source, SemanticProfile::Lua55, compiler_identity())
        .expect_err("nested assignment to a global const must be rejected");
    assert!(
        error.to_string().contains("const variable 'var1'"),
        "{error}"
    );
}

#[test]
fn lua54_and_lua55_reject_multiple_to_be_closed_locals_in_one_list() {
    for source in [
        b"local <close> a, b".as_slice(),
        b"local a<close>, b<close>".as_slice(),
    ] {
        for profile in [SemanticProfile::Lua54, SemanticProfile::Lua55] {
            let error = OwnedCompiler::default()
                .compile(&make_source(source.to_vec()), profile, compiler_identity())
                .expect_err("a Lua local list may have at most one <close> binding");
            assert!(
                error
                    .to_string()
                    .contains("multiple to-be-closed variables"),
                "{profile}: {error}"
            );
        }
    }
}

#[test]
fn lua54_to_be_closed_locals_close_in_reverse_declaration_order() {
    let source = make_source(
        b"local order='' do local <close> first=setmetatable({}, {__close=function() order=order..'a' end}) local <close> second=setmetatable({}, {__close=function() order=order..'b' end}) end return order".to_vec(),
    );
    for profile in [SemanticProfile::Lua54, SemanticProfile::Lua55] {
        let compiled = OwnedCompiler::default()
            .compile(&source, profile, compiler_identity())
            .unwrap();
        assert_eq!(
            Vm::default().execute_blu_v1(compiled.into_validated_artifact(), BluLimits::default()),
            Ok(vec![Value::String(Arc::from(&b"ba"[..]))]),
            "{profile}"
        );
    }
}

#[test]
fn lua54_to_be_closed_handlers_can_yield_and_resume() {
    let source = make_source(
        b"local wrapped=coroutine.wrap(function() local <close> resource=setmetatable({}, {__close=function() return coroutine.yield('closing') end}) return 'done' end) local first=wrapped() local second=wrapped('resumed') return first,second".to_vec(),
    );
    for profile in [SemanticProfile::Lua54, SemanticProfile::Lua55] {
        let compiled = OwnedCompiler::default()
            .compile(&source, profile, compiler_identity())
            .unwrap();
        assert_eq!(
            Vm::default().execute_blu_v1(compiled.into_validated_artifact(), BluLimits::default()),
            Ok(vec![
                Value::String(Arc::from(&b"closing"[..])),
                Value::String(Arc::from(&b"done"[..])),
            ]),
            "{profile}"
        );
    }
}

#[test]
fn lua54_to_be_closed_locals_close_when_goto_exits_their_block() {
    let source = make_source(
        b"local order='' do local <close> resource=setmetatable({}, {__close=function() order='closed' end}) goto done end ::done:: return order".to_vec(),
    );
    for profile in [SemanticProfile::Lua54, SemanticProfile::Lua55] {
        let compiled = OwnedCompiler::default()
            .compile(&source, profile, compiler_identity())
            .unwrap();
        assert_eq!(
            Vm::default().execute_blu_v1(compiled.into_validated_artifact(), BluLimits::default()),
            Ok(vec![Value::String(Arc::from(&b"closed"[..]))]),
            "{profile}"
        );
    }
}

#[test]
fn lua54_to_be_closed_locals_close_when_a_protected_call_raises() {
    let source = make_source(
        b"local error_type='' local ok=pcall(function() local <close> resource=setmetatable({}, {__close=function(value,err) error_type=type(err) end}) error('boom') end) return ok,error_type".to_vec(),
    );
    for profile in [SemanticProfile::Lua54, SemanticProfile::Lua55] {
        let compiled = OwnedCompiler::default()
            .compile(&source, profile, compiler_identity())
            .unwrap();
        assert_eq!(
            Vm::default().execute_blu_v1(compiled.into_validated_artifact(), BluLimits::default()),
            Ok(vec![
                Value::Boolean(false),
                Value::String(Arc::from(&b"string"[..])),
            ]),
            "{profile}"
        );
    }
}

#[test]
fn lua54_error_unwinding_closes_all_handlers_in_reverse_order() {
    let source = make_source(
        b"local order='' local ok=pcall(function() local <close> first=setmetatable({}, {__close=function() order=order..'a' error('close') end}) local <close> second=setmetatable({}, {__close=function() order=order..'b' end}) error('body') end) return ok,order".to_vec(),
    );
    for profile in [SemanticProfile::Lua54, SemanticProfile::Lua55] {
        let compiled = OwnedCompiler::default()
            .compile(&source, profile, compiler_identity())
            .unwrap();
        assert_eq!(
            Vm::default().execute_blu_v1(compiled.into_validated_artifact(), BluLimits::default()),
            Ok(vec![
                Value::Boolean(false),
                Value::String(Arc::from(&b"ba"[..]))
            ]),
            "{profile}"
        );
    }
}

#[test]
fn lua54_top_level_error_unwinding_closes_owned_values() {
    let source = make_source(
        b"closed=0 local resource <close> = setmetatable({}, {__close=function() closed=closed+1 end}) error('body')".to_vec(),
    );
    for profile in [SemanticProfile::Lua54, SemanticProfile::Lua55] {
        let compiled = OwnedCompiler::default()
            .compile(&source, profile, compiler_identity())
            .unwrap();
        let mut vm = Vm::default();
        assert!(matches!(
            vm.execute_blu_v1(compiled.into_validated_artifact(), BluLimits::default()),
            Err(RuntimeError::Raised(Value::String(message))) if message.ends_with(b": body")
        ));
        assert_eq!(vm.global(b"closed"), Some(&Value::Integer(1)), "{profile}");
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
fn owned_parenthesized_call_statements_execute() {
    let source = make_source(
        b"local calls=0 local function invoke() calls=calls+1 end; (invoke)() return calls"
            .to_vec(),
    );
    for profile in SemanticProfile::ALL {
        let compiled = OwnedCompiler::default()
            .compile(&source, profile, compiler_identity())
            .unwrap();
        assert_eq!(
            Vm::default().execute_blu_v1(compiled.into_validated_artifact(), BluLimits::default()),
            Ok(vec![Value::Number(1.0)]),
            "{profile}"
        );
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
            if message.ends_with(b": owned failure")
    ));
}

#[test]
fn native_callbacks_observe_the_active_artifact_profile() {
    let source = make_source(b"return host_profile()".to_vec());
    for profile in SemanticProfile::ALL {
        let compiled = OwnedCompiler::default()
            .compile(&source, profile, compiler_identity())
            .unwrap();
        let mut vm = Vm::default();
        let host_profile = vm.register_function(|vm, _| {
            Ok(vec![Value::String(Arc::from(
                vm.active_semantic_profile()?.to_string().into_bytes(),
            ))])
        });
        vm.set_global(
            b"host_profile".as_slice(),
            Value::NativeFunction(host_profile),
        );

        assert_eq!(
            vm.execute_blu_v1(compiled.into_validated_artifact(), BluLimits::default()),
            Ok(vec![Value::String(Arc::from(
                profile.to_string().into_bytes()
            ))]),
            "{profile}"
        );
    }
}

#[test]
fn version_global_defaults_to_the_active_profile_and_remains_overridable() {
    let source = make_source(b"return _VERSION".to_vec());
    let override_source = make_source(b"_VERSION='custom' return _VERSION".to_vec());
    for profile in SemanticProfile::ALL {
        let compiled = OwnedCompiler::default()
            .compile(&source, profile, compiler_identity())
            .unwrap();
        let expected = match profile {
            SemanticProfile::Blu => &b"Blu"[..],
            SemanticProfile::Luau => &b"Luau"[..],
            SemanticProfile::Lua51 => &b"Lua 5.1"[..],
            SemanticProfile::Lua52 => &b"Lua 5.2"[..],
            SemanticProfile::Lua53 => &b"Lua 5.3"[..],
            SemanticProfile::Lua54 => &b"Lua 5.4"[..],
            SemanticProfile::Lua55 => &b"Lua 5.5"[..],
            _ => unreachable!("SemanticProfile::ALL contains known profiles"),
        };
        assert_eq!(
            Vm::default().execute_blu_v1(compiled.into_validated_artifact(), BluLimits::default()),
            Ok(vec![Value::String(Arc::from(expected))]),
            "{profile}"
        );

        let compiled = OwnedCompiler::default()
            .compile(&override_source, profile, compiler_identity())
            .unwrap();
        assert_eq!(
            Vm::default().execute_blu_v1(compiled.into_validated_artifact(), BluLimits::default()),
            Ok(vec![Value::String(Arc::from(&b"custom"[..]))]),
            "{profile}"
        );
    }
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
        let result =
            Vm::default().execute_blu_v1(compiled.into_validated_artifact(), BluLimits::default());
        assert!(matches!(
            result,
            Err(blu_runtime::RuntimeError::Type { .. })
        ));
    }
}

#[test]
fn blu_mixed_numeric_comparisons_are_exact_beyond_f64_precision() {
    let source = make_source(
        b"local i=9007199254740993 local n=9007199254740992.0 local hi=0x7fffffffffffffff local hif=9223372036854775808.0 local lo=0x8000000000000000 local lof=-9223372036854775808.0 local nan=0/0 local t={[i]='integer',[n]='number'} return i==n,i>n,i<=n,hi==hif,hi<hif,lo==lof,nan==nan,nan<0,nan<=0,t[i],t[n]"
            .to_vec(),
    );
    let compiled = OwnedCompiler::default()
        .compile(&source, SemanticProfile::Blu, compiler_identity())
        .unwrap();
    assert_eq!(
        Vm::default().execute_blu_v1(compiled.into_validated_artifact(), BluLimits::default()),
        Ok(vec![
            Value::Boolean(false),
            Value::Boolean(true),
            Value::Boolean(false),
            Value::Boolean(false),
            Value::Boolean(true),
            Value::Boolean(true),
            Value::Boolean(false),
            Value::Boolean(false),
            Value::Boolean(false),
            Value::String(Arc::from(&b"integer"[..])),
            Value::String(Arc::from(&b"number"[..])),
        ])
    );
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

    let argument_count = make_source(
        b"local value = setmetatable({}, {__len = function(...) return select('#', ...) end}) return #value"
            .to_vec(),
    );
    for profile in SemanticProfile::ALL {
        let compiled = OwnedCompiler::default()
            .compile(&argument_count, profile, compiler_identity())
            .unwrap();
        let result =
            Vm::default().execute_blu_v1(compiled.into_validated_artifact(), BluLimits::default());
        assert_eq!(
            result,
            Ok(vec![if profile == SemanticProfile::Lua51 {
                Value::Number(0.0)
            } else if matches!(
                profile,
                SemanticProfile::Lua53 | SemanticProfile::Lua54 | SemanticProfile::Lua55
            ) {
                Value::Integer(2)
            } else if profile == SemanticProfile::Luau {
                Value::Number(1.0)
            } else {
                Value::Number(2.0)
            }]),
            "{profile}"
        );
    }
}

#[test]
fn owned_protected_memory_errors_use_the_guest_diagnostic_for_blu_and_luau() {
    let source = make_source(
        b"local ok, error = pcall(function() table.create(1000000) end) return ok, error".to_vec(),
    );
    let memory = MemoryConfig {
        hard_limit_bytes: Some(8 * 1024 * 1024),
        gc_start_bytes: 8 * 1024 * 1024,
        max_single_allocation_bytes: 1 << 20,
        ..MemoryConfig::default()
    };
    for profile in [SemanticProfile::Blu, SemanticProfile::Luau] {
        let compiled = OwnedCompiler::default()
            .compile(&source, profile, compiler_identity())
            .unwrap();
        let result = Vm::try_new_with_memory(dialect(profile), memory)
            .unwrap()
            .execute_blu_v1(compiled.into_validated_artifact(), BluLimits::default());
        assert_eq!(
            result,
            Ok(vec![
                Value::Boolean(false),
                Value::String(Arc::from(&b"not enough memory"[..])),
            ]),
            "{profile}"
        );
    }
}

#[test]
fn luau_traceback_retains_the_protected_caller_after_a_yield() {
    let source = make_source(
        b"local function target() coroutine.yield() error('boom') end\nlocal co = coroutine.create(function() return xpcall(target, debug.traceback) end)\nlocal started = coroutine.resume(co)\nlocal resumed, ok, message = coroutine.resume(co)\nlocal first = type(message) == 'string' and string.find(message, 'owned-slice.blu:', 1, true)\nlocal second = first and string.find(message, 'owned-slice.blu:', first + 1, true)\nlocal third = second and string.find(message, 'owned-slice.blu:', second + 1, true)\nlocal fourth = third and string.find(message, 'owned-slice.blu:', third + 1, true)\nreturn started and resumed and not ok and first ~= nil and second ~= nil and third ~= nil and fourth == nil".to_vec(),
    );
    let compiled = OwnedCompiler::default()
        .compile(&source, SemanticProfile::Luau, compiler_identity())
        .unwrap();
    assert_eq!(
        Vm::default().execute_blu_v1(compiled.into_validated_artifact(), BluLimits::default()),
        Ok(vec![Value::Boolean(true)])
    );
}

#[test]
fn decimal_constants_follow_each_profile_numeric_policy() {
    let number_source = make_source(b"return 9007199254740993, 18446744073709551616".to_vec());
    for profile in [
        SemanticProfile::Lua51,
        SemanticProfile::Lua52,
        SemanticProfile::Luau,
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
                | if profile == SemanticProfile::Lua52 {
                    FeatureBits::IMPLICIT_ENVIRONMENT
                } else {
                    FeatureBits::empty()
                }
        );
    }

    let integer_then_float =
        make_source(b"return 9223372036854775807, 9223372036854775808".to_vec());
    for profile in [
        SemanticProfile::Blu,
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
            FeatureBits::BASELINE
                | FeatureBits::INTEGER_CONSTANTS
                | if matches!(
                    profile,
                    SemanticProfile::Lua53 | SemanticProfile::Lua54 | SemanticProfile::Lua55
                ) {
                    FeatureBits::IMPLICIT_ENVIRONMENT
                } else {
                    FeatureBits::empty()
                }
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
        FeatureBits::BASELINE | FeatureBits::IMPLICIT_ENVIRONMENT
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
fn blu_integer_arithmetic_wraps_and_mixed_arithmetic_promotes() {
    let source = make_source(
        b"return 0x7fffffffffffffff+1,0x8000000000000000-1,0x4000000000000000*4,5+0.5,5/2,-7%3"
            .to_vec(),
    );
    let compiled = OwnedCompiler::default()
        .compile(&source, SemanticProfile::Blu, compiler_identity())
        .unwrap();
    assert_eq!(
        Vm::default().execute_blu_v1(compiled.into_validated_artifact(), BluLimits::default()),
        Ok(vec![
            Value::Integer(i64::MIN),
            Value::Integer(i64::MAX),
            Value::Integer(0),
            Value::Number(5.5),
            Value::Number(2.5),
            Value::Integer(2),
        ])
    );
}

#[test]
fn arithmetic_numeric_string_coercion_is_profile_typed() {
    let shared = make_source(b"return '3'+1,'0x10'+1,' 5 '*2".to_vec());
    for profile in SemanticProfile::ALL {
        let compiled = OwnedCompiler::default()
            .compile(&shared, profile, compiler_identity())
            .unwrap();
        let integer = matches!(
            profile,
            SemanticProfile::Blu | SemanticProfile::Lua54 | SemanticProfile::Lua55
        );
        let value = |integer_value, number_value| {
            if integer {
                Value::Integer(integer_value)
            } else {
                Value::Number(number_value)
            }
        };
        assert_eq!(
            Vm::default().execute_blu_v1(compiled.into_validated_artifact(), BluLimits::default()),
            Ok(vec![value(4, 4.0), value(17, 17.0), value(10, 10.0)]),
            "{profile}"
        );
    }

    let floor = make_source(b"return '7'//3".to_vec());
    for profile in [
        SemanticProfile::Blu,
        SemanticProfile::Luau,
        SemanticProfile::Lua53,
        SemanticProfile::Lua54,
        SemanticProfile::Lua55,
    ] {
        let compiled = OwnedCompiler::default()
            .compile(&floor, profile, compiler_identity())
            .unwrap();
        let expected = if matches!(
            profile,
            SemanticProfile::Blu | SemanticProfile::Lua54 | SemanticProfile::Lua55
        ) {
            Value::Integer(2)
        } else {
            Value::Number(2.0)
        };
        assert_eq!(
            Vm::default().execute_blu_v1(compiled.into_validated_artifact(), BluLimits::default()),
            Ok(vec![expected]),
            "{profile}"
        );
    }

    let invalid = make_source(b"return 'not-a-number'+1".to_vec());
    for profile in SemanticProfile::ALL {
        let compiled = OwnedCompiler::default()
            .compile(&invalid, profile, compiler_identity())
            .unwrap();
        assert!(matches!(
            Vm::default().execute_blu_v1(compiled.into_validated_artifact(), BluLimits::default()),
            Err(RuntimeError::Type {
                operation: "arithmetic",
                actual: "string",
                ..
            })
        ));
    }
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
            SemanticProfile::Blu
                | SemanticProfile::Lua53
                | SemanticProfile::Lua54
                | SemanticProfile::Lua55
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
            let integer = profile == SemanticProfile::Blu;
            assert_eq!(
                compiled.artifact().prototypes()[0].constants.as_slice(),
                [
                    if integer {
                        Constant::Integer(1_000)
                    } else {
                        Constant::Number(1_000.0)
                    },
                    Constant::Number(12_345.125),
                    if integer {
                        Constant::Integer(65_535)
                    } else {
                        Constant::Number(65_535.0)
                    },
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
    let source = make_source(b"return 0b101010,0B1111_0000".to_vec());
    for profile in SemanticProfile::ALL {
        let result = OwnedCompiler::default().compile(&source, profile, compiler_identity());
        if matches!(profile, SemanticProfile::Blu | SemanticProfile::Luau) {
            let compiled = result.unwrap();
            let constants = if profile == SemanticProfile::Blu {
                [Constant::Integer(42), Constant::Integer(240)]
            } else {
                [Constant::Number(42.0), Constant::Number(240.0)]
            };
            assert_eq!(
                compiled.artifact().prototypes()[0].constants.as_slice(),
                constants,
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
fn binary_integer_boundaries_are_explicit_for_blu_and_luau() {
    let source = make_source(
        b"return 0b11111111_11111111_11111111_11111111_11111111_11111111_11111111_11111111,0b1_00000000_00000000_00000000_00000000_00000000_00000000_00000000_00000000"
            .to_vec(),
    );
    for (profile, expected) in [
        (
            SemanticProfile::Blu,
            [Constant::Integer(-1), Constant::Integer(0)],
        ),
        (
            SemanticProfile::Luau,
            [
                Constant::Number(18_446_744_073_709_551_615.0),
                Constant::Number(18_446_744_073_709_551_616.0),
            ],
        ),
    ] {
        let compiled = OwnedCompiler::default()
            .compile(&source, profile, compiler_identity())
            .unwrap();
        assert_eq!(compiled.artifact().main().constants, expected, "{profile}");
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
fn lua_long_strings_collapse_lfcr_newline_pairs() {
    let source = make_source(b"return [[\ralo\n\ralo\r\n]]".to_vec());
    let compiled = OwnedCompiler::default()
        .compile(&source, SemanticProfile::Lua54, compiler_identity())
        .unwrap();
    assert_eq!(
        compiled.artifact().main().constants,
        [Constant::String(b"alo\nalo\n".to_vec())]
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
        b"return string.find(string.rep('a', 4000), string.rep('a', 3000) .. '[b]')".to_vec(),
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
fn string_gsub_supports_bounded_table_replacements_and_index_handlers() {
    let source = make_source(
        b"local first,n=string.gsub('cat dog bird fox','%a+',{cat='C',dog=false,bird=7}) local second,m=string.gsub('ab','(.)',{a='A',b='B'}) local third,k=string.gsub('a','()a',{[1]='X'}) return first,n,second,m,third,k"
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
        let count = |value| {
            if modern {
                Value::Integer(value)
            } else {
                Value::Number(value as f64)
            }
        };
        assert_eq!(
            Vm::default().execute_blu_v1(compiled.into_validated_artifact(), BluLimits::default()),
            Ok(vec![
                Value::String(Arc::from(&b"C dog 7 fox"[..])),
                count(4),
                Value::String(Arc::from(&b"AB"[..])),
                count(2),
                Value::String(Arc::from(&b"X"[..])),
                count(1),
            ]),
            "{profile}"
        );

        let source = make_source(
            b"local replacement=setmetatable({}, {__index=function() return 'X' end}) return string.gsub('a','a',replacement)"
                .to_vec(),
        );
        let compiled = OwnedCompiler::default()
            .compile(&source, profile, compiler_identity())
            .unwrap();
        assert_eq!(
            Vm::default().execute_blu_v1(compiled.into_validated_artifact(), BluLimits::default()),
            Ok(vec![Value::String(Arc::from(&b"X"[..])), count(1)]),
            "{profile}"
        );
    }
}

#[test]
fn string_gsub_supports_non_yielding_function_replacements() {
    let source = make_source(
        b"local a,b=string.gsub('a1b2','(%a)(%d)',function(letter,digit) return digit..letter end) local c,d=string.gsub('ab','(.)',function(value) if value=='a' then return false end return value..value end) return a,b,c,d".to_vec(),
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
                Value::String(Arc::from(&b"1a2b"[..])),
                count(2),
                Value::String(Arc::from(&b"abb"[..])),
                count(2),
            ]),
            "{profile}"
        );
    }
}

#[test]
fn string_gsub_resumes_owned_callbacks_and_preserves_operation_state() {
    let source = make_source(
        b"local wrapped=coroutine.wrap(function() local result,count=string.gsub('aa','a',function(value) local replacement=coroutine.yield(value) collectgarbage('collect') return replacement end) return result,count end) local first=wrapped() local second=wrapped('X') local result,count=wrapped('Y') return first,second,result,count"
            .to_vec(),
    );
    for profile in SemanticProfile::ALL {
        let compiled = OwnedCompiler::default()
            .compile(&source, profile, compiler_identity())
            .unwrap();
        let count = if matches!(
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
        assert_eq!(
            Vm::default().execute_blu_v1(compiled.into_validated_artifact(), BluLimits::default()),
            Ok(vec![
                Value::String(Arc::from(&b"a"[..])),
                Value::String(Arc::from(&b"a"[..])),
                Value::String(Arc::from(&b"XY"[..])),
                count,
            ]),
            "{profile}"
        );
    }
}

#[test]
fn string_gmatch_returns_a_stateful_function_iterator() {
    let source = make_source(
        b"local iterator=string.gmatch('a1b2','(%a)(%d)') local first,first_digit=iterator() local second,second_digit=iterator() local done=iterator() return type(iterator),first,first_digit,second,second_digit,done".to_vec(),
    );
    for profile in SemanticProfile::ALL {
        let compiled = OwnedCompiler::default()
            .compile(&source, profile, compiler_identity())
            .unwrap();
        assert_eq!(
            Vm::default().execute_blu_v1(compiled.into_validated_artifact(), BluLimits::default()),
            Ok(vec![
                Value::String(Arc::from(&b"function"[..])),
                Value::String(Arc::from(&b"a"[..])),
                Value::String(Arc::from(&b"1"[..])),
                Value::String(Arc::from(&b"b"[..])),
                Value::String(Arc::from(&b"2"[..])),
                Value::Nil,
            ]),
            "{profile}"
        );
        if matches!(
            profile,
            SemanticProfile::Blu
                | SemanticProfile::Luau
                | SemanticProfile::Lua51
                | SemanticProfile::Lua52
                | SemanticProfile::Lua53
        ) {
            let source = make_source(
                b"local collected='' for letter,digit in string.gmatch('a1b2','(%a)(%d)') do collected=collected..letter..digit end return collected".to_vec(),
            );
            let compiled = OwnedCompiler::default()
                .compile(&source, profile, compiler_identity())
                .unwrap();
            assert_eq!(
                Vm::default()
                    .execute_blu_v1(compiled.into_validated_artifact(), BluLimits::default()),
                Ok(vec![Value::String(Arc::from(&b"a1b2"[..]))]),
                "{profile}"
            );
        }
    }
}

#[test]
fn string_gsub_rejects_unimplemented_capture_replacements_structurally() {
    for profile in SemanticProfile::ALL {
        let source = make_source(b"return string.gsub('a','(a)','%2')".to_vec());
        let compiled = OwnedCompiler::default()
            .compile(&source, profile, compiler_identity())
            .unwrap();
        assert!(matches!(
            Vm::default().execute_blu_v1(compiled.into_validated_artifact(), BluLimits::default()),
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
                if matches!(profile, SemanticProfile::Blu | SemanticProfile::Luau) {
                    "unsupported"
                } else {
                    "supported"
                },
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
                "supported" => assert_eq!(
                    result,
                    Ok(vec![if matches!(
                        profile,
                        SemanticProfile::Lua53 | SemanticProfile::Lua54 | SemanticProfile::Lua55
                    ) {
                        Value::Integer(0)
                    } else {
                        Value::Number(0.0)
                    }])
                ),
                "type" => assert!(matches!(result, Err(RuntimeError::Type { .. }))),
                _ => unreachable!(),
            }
        }
    }
}

#[test]
fn collectgarbage_controls_follow_lua_profile_surface() {
    for profile in SemanticProfile::ALL {
        let source = match profile {
            SemanticProfile::Lua51 => {
                b"local stopped,stop_value=pcall(collectgarbage,'stop') local running=pcall(collectgarbage,'isrunning') local collected,collect_value=pcall(collectgarbage,'collect') local restarted,restart_value=pcall(collectgarbage,'restart') return stopped and type(stop_value)=='number' and stop_value==0 and not running and collected and type(collect_value)=='number' and restarted and type(restart_value)=='number' and restart_value==0".as_slice()
            }
            SemanticProfile::Lua52
            | SemanticProfile::Lua53
            | SemanticProfile::Lua54
            | SemanticProfile::Lua55 => {
                br#"local stopped,stop_value=pcall(collectgarbage,'stop')
if not stopped or stop_value~=0 then return false end
local was_stopped,state=pcall(collectgarbage,'isrunning')
if not was_stopped or state~=false then return false end
local collected,collect_value=pcall(collectgarbage,'collect')
if not collected or collect_value~=0 then return false end
local restarted,restart_value=pcall(collectgarbage,'restart')
if not restarted or restart_value~=0 then return false end
local running,after=pcall(collectgarbage,'isrunning')
return running and after==true"#
            }
            SemanticProfile::Blu | SemanticProfile::Luau => {
                b"return not pcall(collectgarbage,'stop') and pcall(collectgarbage,'collect') and not pcall(collectgarbage,'restart') and not pcall(collectgarbage,'isrunning')".as_slice()
            }
            _ => unreachable!("SemanticProfile::ALL contains only known profiles"),
        };
        let source = make_source(source.to_vec());
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
fn collectgarbage_step_reports_reclaimed_memory() {
    let source = make_source(
        br#"collectgarbage('collect')
local values = {}
for index = 1, 100 do values[index] = {{}}; local garbage = {} end
local before = collectgarbage('count')
collectgarbage('stop')
local completed = collectgarbage('step', 20000)
local after = collectgarbage('count')
return before, after, completed"#
            .to_vec(),
    );
    for profile in [SemanticProfile::Lua54, SemanticProfile::Lua55] {
        let compiled = OwnedCompiler::default()
            .compile(&source, profile, compiler_identity())
            .unwrap();
        let mut vm = Vm::default();
        let result = vm
            .execute_blu_v1(compiled.into_validated_artifact(), BluLimits::default())
            .unwrap_or_else(|error| panic!("{profile}: {error}"));
        assert!(
            matches!(result.get(2), Some(Value::Boolean(true))),
            "{profile}"
        );
        assert!(
            matches!((&result[0], &result[1]),
                (Value::Number(before), Value::Number(after)) if before > after),
            "{profile}: expected collection to reduce the public memory count, got {result:?}"
        );
    }
}

#[test]
fn collectgarbage_step_size_controls_completion_budget() {
    let source = make_source(
        br#"collectgarbage('stop')
local function steps(size)
    collectgarbage('collect')
    local values = {}
    for index = 1, 100 do values[index] = {{}} end
    local count = 0
    repeat count = count + 1 until collectgarbage('step', size)
    return count
end
return steps(10) < steps(2)"#
            .to_vec(),
    );
    for profile in [SemanticProfile::Lua54, SemanticProfile::Lua55] {
        let compiled = OwnedCompiler::default()
            .compile(&source, profile, compiler_identity())
            .unwrap();
        assert_eq!(
            Vm::default()
                .execute_blu_v1(compiled.into_validated_artifact(), BluLimits::default())
                .unwrap_or_else(|error| panic!("{profile}: {error}")),
            vec![Value::Boolean(true)],
            "{profile}"
        );
    }
}

#[test]
fn modern_collectgarbage_modes_and_parameters_round_trip() {
    let mode_source = make_source(
        b"local first=collectgarbage('incremental') local second=collectgarbage('generational') local third=collectgarbage('incremental') return first=='incremental' and second=='incremental' and third=='generational'".to_vec(),
    );
    for profile in [SemanticProfile::Lua54, SemanticProfile::Lua55] {
        let compiled = OwnedCompiler::default()
            .compile(&mode_source, profile, compiler_identity())
            .unwrap();
        assert_eq!(
            Vm::default()
                .execute_blu_v1(compiled.into_validated_artifact(), BluLimits::default())
                .unwrap_or_else(|error| panic!("{profile}: {error}")),
            vec![Value::Boolean(true)],
            "{profile}"
        );
    }

    let parameter_source = make_source(
        b"local old=collectgarbage('param','pause',100) local pause=collectgarbage('param','pause') local oldmul=collectgarbage('param','stepmul',100) local stepmul=collectgarbage('param','stepmul') return old==200 and pause==100 and oldmul==200 and stepmul==100".to_vec(),
    );
    let compiled = OwnedCompiler::default()
        .compile(
            &parameter_source,
            SemanticProfile::Lua55,
            compiler_identity(),
        )
        .unwrap();
    assert_eq!(
        Vm::default()
            .execute_blu_v1(compiled.into_validated_artifact(), BluLimits::default())
            .unwrap(),
        vec![Value::Boolean(true)]
    );
}

#[test]
fn lua52_collectgarbage_modes_and_major_increment_round_trip() {
    let source = make_source(
        b"local gen=collectgarbage('generational') local inc=collectgarbage('incremental') local old=collectgarbage('setmajorinc',250) local prior=collectgarbage('setmajorinc',300) return gen==0 and inc==0 and old==200 and prior==250".to_vec(),
    );
    let compiled = OwnedCompiler::default()
        .compile(&source, SemanticProfile::Lua52, compiler_identity())
        .unwrap();
    assert_eq!(
        Vm::default()
            .execute_blu_v1(compiled.into_validated_artifact(), BluLimits::default())
            .unwrap(),
        vec![Value::Boolean(true)]
    );
}

#[test]
fn stopped_collectgarbage_disables_automatic_collection_until_restart() {
    for profile in [
        SemanticProfile::Lua51,
        SemanticProfile::Lua52,
        SemanticProfile::Lua53,
        SemanticProfile::Lua54,
        SemanticProfile::Lua55,
    ] {
        let source = make_source(
            br#"collectgarbage('stop')
local allocated = pcall(function()
    local value
    for _=1,1000 do value={} end
end)
local running_ok,running = pcall(collectgarbage,'isrunning')
local stopped = (not running_ok) or running == false
collectgarbage('restart')
local restarted_ok,restarted = pcall(collectgarbage,'isrunning')
return allocated, stopped, (not restarted_ok) or restarted"#
                .to_vec(),
        );
        let compiled = OwnedCompiler::default()
            .compile(&source, profile, compiler_identity())
            .unwrap();
        let baseline = Vm::default().heap().live_objects();
        let result = Vm::default()
            .with_heap_object_limit(baseline + 32)
            .execute_blu_v1(compiled.into_validated_artifact(), BluLimits::default())
            .unwrap_or_else(|error| panic!("{profile}: {error}"));
        assert_eq!(
            result,
            vec![
                Value::Boolean(false),
                Value::Boolean(true),
                Value::Boolean(true),
            ],
            "{profile}"
        );
    }
}

#[test]
fn collectgarbage_stop_and_restart_return_profile_numeric_subtypes() {
    let source = make_source(b"return collectgarbage('stop'), collectgarbage('restart')".to_vec());
    for profile in [
        SemanticProfile::Lua51,
        SemanticProfile::Lua52,
        SemanticProfile::Lua53,
        SemanticProfile::Lua54,
        SemanticProfile::Lua55,
    ] {
        let compiled = OwnedCompiler::default()
            .compile(&source, profile, compiler_identity())
            .unwrap();
        let zero = if matches!(
            profile,
            SemanticProfile::Lua53 | SemanticProfile::Lua54 | SemanticProfile::Lua55
        ) {
            Value::Integer(0)
        } else {
            Value::Number(0.0)
        };
        assert_eq!(
            Vm::default().execute_blu_v1(compiled.into_validated_artifact(), BluLimits::default()),
            Ok(vec![zero.clone(), zero]),
            "{profile}"
        );
    }
}

#[test]
fn table_gc_finalizers_follow_lua_profile_availability() {
    let source = make_source(
        b"local finalized=0 local function make() local value=setmetatable({}, {__gc=function() finalized=finalized+1 end}) end make() collectgarbage('collect') return finalized".to_vec(),
    );
    for profile in SemanticProfile::ALL {
        let compiled = OwnedCompiler::default()
            .compile(&source, profile, compiler_identity())
            .unwrap();
        let expected = match profile {
            SemanticProfile::Blu => Value::Integer(0),
            SemanticProfile::Lua52 => Value::Number(1.0),
            SemanticProfile::Lua53 | SemanticProfile::Lua54 | SemanticProfile::Lua55 => {
                Value::Integer(1)
            }
            SemanticProfile::Lua51 | SemanticProfile::Luau => Value::Number(0.0),
            _ => unreachable!("SemanticProfile::ALL contains only known profiles"),
        };
        assert_eq!(
            Vm::default().execute_blu_v1(compiled.into_validated_artifact(), BluLimits::default()),
            Ok(vec![expected]),
            "{profile}"
        );
    }
}

#[test]
fn automatic_table_finalizers_preserve_captured_open_locals() {
    let source = make_source(
        b"local finished=false local value=setmetatable({}, {__gc=function() finished=true end}) repeat value={} until finished return finished".to_vec(),
    );
    for profile in [
        SemanticProfile::Lua52,
        SemanticProfile::Lua53,
        SemanticProfile::Lua54,
        SemanticProfile::Lua55,
    ] {
        let compiled = OwnedCompiler::default()
            .compile(&source, profile, compiler_identity())
            .unwrap();
        let mut vm = Vm::try_new_with_memory(
            Dialect::Blu,
            MemoryConfig {
                gc_start_bytes: 1,
                gc_growth_percent: 0,
                ..MemoryConfig::default()
            },
        )
        .unwrap()
        .with_instruction_limit(100_000);
        assert_eq!(
            vm.execute_blu_v1(compiled.into_validated_artifact(), BluLimits::default()),
            Ok(vec![Value::Boolean(true)]),
            "{profile}"
        );
    }
}

#[test]
fn table_finalizers_arm_from_late_non_nil_gc_markers() {
    let source = make_source(
        b"local finalized=0 local value=setmetatable({}, {__gc=true}) local metatable=getmetatable(value) metatable.__gc=function() finalized=10 end value=nil collectgarbage() return finalized==10".to_vec(),
    );
    for profile in [
        SemanticProfile::Lua52,
        SemanticProfile::Lua53,
        SemanticProfile::Lua54,
        SemanticProfile::Lua55,
    ] {
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
fn table_gc_finalizers_can_resurrect_once_and_do_not_repeat() {
    let source = make_source(
        b"local finalized=0 local resurrected local function make() local value=setmetatable({}, {__gc=function(value) finalized=finalized+1 resurrected=value end}) end make() collectgarbage('collect') local first=finalized==1 and resurrected~=nil collectgarbage('collect') return first and finalized==1 and resurrected~=nil".to_vec(),
    );
    for profile in SemanticProfile::ALL {
        let compiled = OwnedCompiler::default()
            .compile(&source, profile, compiler_identity())
            .unwrap();
        assert_eq!(
            Vm::default().execute_blu_v1(compiled.into_validated_artifact(), BluLimits::default()),
            Ok(vec![Value::Boolean(matches!(
                profile,
                SemanticProfile::Lua52
                    | SemanticProfile::Lua53
                    | SemanticProfile::Lua54
                    | SemanticProfile::Lua55
            ))]),
            "{profile}"
        );
    }
}

#[test]
fn table_gc_finalizers_can_be_explicitly_rearmed_after_resurrection() {
    let source = make_source(
        b"local finalized=0 local resurrected local metatable local function finalize(value) finalized=finalized+1 resurrected=value setmetatable(value, metatable) end metatable={__gc=finalize} local function make() local value=setmetatable({}, metatable) end make() collectgarbage('collect') local first=finalized==1 and resurrected~=nil resurrected=nil collectgarbage('collect') return first and finalized==2".to_vec(),
    );
    for profile in SemanticProfile::ALL {
        let compiled = OwnedCompiler::default()
            .compile(&source, profile, compiler_identity())
            .unwrap();
        let result =
            Vm::default().execute_blu_v1(compiled.into_validated_artifact(), BluLimits::default());
        assert_eq!(
            result,
            Ok(vec![Value::Boolean(matches!(
                profile,
                SemanticProfile::Lua53 | SemanticProfile::Lua54 | SemanticProfile::Lua55
            ))]),
            "{profile}"
        );
    }
}

#[test]
fn table_gc_finalizers_follow_reverse_order_and_error_policy() {
    let order_source = make_source(
        b"local order={} local metatable={__gc=function(value) order[#order+1]=value.id end} local function make(id) local value=setmetatable({id=id}, metatable) end make(1) make(2) make(3) collectgarbage('collect') return table.concat(order, ',')".to_vec(),
    );
    let error_source = make_source(
        b"local finalized=0 local function make() local value=setmetatable({}, {__gc=function() finalized=finalized+1 error('boom') end}) end make() local ok, err=pcall(collectgarbage, 'collect') return ok, type(err), finalized".to_vec(),
    );
    let yield_source = make_source(
        b"local finalized=0 local function make() local value=setmetatable({}, {__gc=function() finalized=finalized+1 coroutine.yield('yielded') finalized=finalized+1 end}) end make() local ok, err=pcall(collectgarbage, 'collect') return ok, type(err), finalized".to_vec(),
    );
    for profile in SemanticProfile::ALL {
        let order = OwnedCompiler::default()
            .compile(&order_source, profile, compiler_identity())
            .unwrap();
        let order_expected = if matches!(
            profile,
            SemanticProfile::Lua52
                | SemanticProfile::Lua53
                | SemanticProfile::Lua54
                | SemanticProfile::Lua55
        ) {
            "3,2,1"
        } else {
            ""
        };
        assert_eq!(
            Vm::default().execute_blu_v1(order.into_validated_artifact(), BluLimits::default()),
            Ok(vec![Value::String(Arc::from(order_expected.as_bytes()))]),
            "order {profile}"
        );

        let error = OwnedCompiler::default()
            .compile(&error_source, profile, compiler_identity())
            .unwrap();
        let expected = match profile {
            SemanticProfile::Lua52 | SemanticProfile::Lua53 => vec![
                Value::Boolean(false),
                Value::String(Arc::from(&b"string"[..])),
                Value::Number(1.0),
            ],
            SemanticProfile::Lua54 | SemanticProfile::Lua55 => vec![
                Value::Boolean(true),
                Value::String(Arc::from(&b"number"[..])),
                Value::Integer(1),
            ],
            SemanticProfile::Blu | SemanticProfile::Luau | SemanticProfile::Lua51 => vec![
                Value::Boolean(true),
                if profile == SemanticProfile::Luau {
                    Value::String(Arc::from(&b"nil"[..]))
                } else {
                    Value::String(Arc::from(&b"number"[..]))
                },
                if matches!(profile, SemanticProfile::Blu) {
                    Value::Integer(0)
                } else {
                    Value::Number(0.0)
                },
            ],
            _ => unreachable!("SemanticProfile::ALL contains only known profiles"),
        };
        assert_eq!(
            Vm::default().execute_blu_v1(error.into_validated_artifact(), BluLimits::default()),
            Ok(expected),
            "error {profile}"
        );

        let yielded = OwnedCompiler::default()
            .compile(&yield_source, profile, compiler_identity())
            .unwrap();
        let expected = match profile {
            SemanticProfile::Lua52 | SemanticProfile::Lua53 => vec![
                Value::Boolean(false),
                Value::String(Arc::from(&b"string"[..])),
                Value::Number(1.0),
            ],
            SemanticProfile::Lua54 | SemanticProfile::Lua55 => vec![
                Value::Boolean(true),
                Value::String(Arc::from(&b"number"[..])),
                Value::Integer(1),
            ],
            SemanticProfile::Blu => vec![
                Value::Boolean(true),
                Value::String(Arc::from(&b"number"[..])),
                Value::Integer(0),
            ],
            SemanticProfile::Luau => vec![
                Value::Boolean(true),
                Value::String(Arc::from(&b"nil"[..])),
                Value::Number(0.0),
            ],
            SemanticProfile::Lua51 => vec![
                Value::Boolean(true),
                Value::String(Arc::from(&b"number"[..])),
                Value::Number(0.0),
            ],
            _ => unreachable!("SemanticProfile::ALL contains only known profiles"),
        };
        assert_eq!(
            Vm::default().execute_blu_v1(yielded.into_validated_artifact(), BluLimits::default()),
            Ok(expected),
            "yield {profile}"
        );
    }
}

#[test]
fn table_gc_finalizers_keep_the_profile_for_host_triggered_collection() {
    let source = make_source(
        b"finalized=0 return setmetatable({}, {__gc=function() finalized=finalized+1 end})"
            .to_vec(),
    );
    for profile in SemanticProfile::ALL {
        let compiled = OwnedCompiler::default()
            .compile(&source, profile, compiler_identity())
            .unwrap();
        let mut vm = Vm::default();
        let values = vm
            .execute_blu_v1(compiled.into_validated_artifact(), BluLimits::default())
            .unwrap();
        assert_eq!(values.len(), 1, "{profile}");
        assert!(vm.release_value(&values[0]), "{profile}");
        vm.collect(core::iter::empty()).unwrap();
        let expected = if matches!(
            profile,
            SemanticProfile::Lua52
                | SemanticProfile::Lua53
                | SemanticProfile::Lua54
                | SemanticProfile::Lua55
        ) {
            if matches!(
                profile,
                SemanticProfile::Lua53 | SemanticProfile::Lua54 | SemanticProfile::Lua55
            ) {
                Value::Integer(1)
            } else {
                Value::Number(1.0)
            }
        } else if profile == SemanticProfile::Blu {
            Value::Integer(0)
        } else {
            Value::Number(0.0)
        };
        assert_eq!(vm.global(b"finalized"), Some(&expected), "{profile}");
    }
}

#[test]
fn table_gc_finalizer_register_liveness_boundary_is_explicit() {
    let source = make_source(
        b"local finalized=0 local resurrected local metatable local function finalize(value) finalized=finalized+1 resurrected=value end metatable={__gc=finalize} local function make() local value=setmetatable({}, metatable) end make() collectgarbage('collect') local function rearm(value) setmetatable(value, metatable) end if resurrected~=nil then rearm(resurrected) resurrected=nil collectgarbage('collect') end return finalized".to_vec(),
    );
    for profile in SemanticProfile::ALL {
        let compiled = OwnedCompiler::default()
            .compile(&source, profile, compiler_identity())
            .unwrap();
        let expected = match profile {
            SemanticProfile::Lua52 => Value::Number(1.0),
            SemanticProfile::Lua53 | SemanticProfile::Lua54 | SemanticProfile::Lua55 => {
                Value::Integer(2)
            }
            SemanticProfile::Blu => Value::Integer(0),
            SemanticProfile::Luau | SemanticProfile::Lua51 => Value::Number(0.0),
            _ => unreachable!("SemanticProfile::ALL contains only known profiles"),
        };
        assert_eq!(
            Vm::default().execute_blu_v1(compiled.into_validated_artifact(), BluLimits::default()),
            Ok(vec![expected]),
            "{profile}"
        );
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
fn numeric_libraries_preserve_exact_mixed_ordering() {
    let source = make_source(
        b"local i=9007199254740993 local n=9007199254740992.0 local hi=0x7fffffffffffffff local hif=9223372036854775808.0 local values={i,n,hi,hif} table.sort(values) return math.min(i,n),math.max(i,n),values[1],values[2],values[3],values[4]"
            .to_vec(),
    );
    let compiled = OwnedCompiler::default()
        .compile(&source, SemanticProfile::Blu, compiler_identity())
        .unwrap();
    assert_eq!(
        Vm::default().execute_blu_v1(compiled.into_validated_artifact(), BluLimits::default()),
        Ok(vec![
            Value::Number(9_007_199_254_740_992.0),
            Value::Integer(9_007_199_254_740_993),
            Value::Number(9_007_199_254_740_992.0),
            Value::Integer(9_007_199_254_740_993),
            Value::Integer(i64::MAX),
            Value::Number(9_223_372_036_854_775_808.0),
        ])
    );
}

#[test]
fn table_sort_supports_custom_comparators_and_rejects_unordered_defaults() {
    for profile in SemanticProfile::ALL {
        let source = make_source(
            b"local values={2,1} local result=table.sort(values,function(a,b) return a>b end) return values[1],values[2],result".to_vec(),
        );
        let compiled = OwnedCompiler::default()
            .compile(&source, profile, compiler_identity())
            .unwrap();
        let result =
            Vm::default().execute_blu_v1(compiled.into_validated_artifact(), BluLimits::default());
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
        let one = if matches!(
            profile,
            SemanticProfile::Blu
                | SemanticProfile::Lua53
                | SemanticProfile::Lua54
                | SemanticProfile::Lua55
        ) {
            Value::Integer(1)
        } else {
            Value::Number(1.0)
        };
        assert_eq!(result, Ok(vec![two, one, Value::Nil]), "{profile}");

        let source = make_source(
            b"local mt={__lt=function(a,b) return a[1]<b[1] end} local values={setmetatable({2},mt),setmetatable({1},mt)} table.sort(values) return values[1][1],values[2][1]".to_vec(),
        );
        let compiled = OwnedCompiler::default()
            .compile(&source, profile, compiler_identity())
            .unwrap();
        let result =
            Vm::default().execute_blu_v1(compiled.into_validated_artifact(), BluLimits::default());
        let sorted_one = if matches!(
            profile,
            SemanticProfile::Blu
                | SemanticProfile::Lua53
                | SemanticProfile::Lua54
                | SemanticProfile::Lua55
        ) {
            Value::Integer(1)
        } else {
            Value::Number(1.0)
        };
        let sorted_two = if matches!(
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
        assert_eq!(result, Ok(vec![sorted_one, sorted_two]), "{profile}");

        let source = make_source(b"return table.sort({1,'a'})".to_vec());
        let compiled = OwnedCompiler::default()
            .compile(&source, profile, compiler_identity())
            .unwrap();
        let result =
            Vm::default().execute_blu_v1(compiled.into_validated_artifact(), BluLimits::default());
        assert!(
            matches!(result, Err(RuntimeError::Type { .. })),
            "{profile}"
        );
    }
}

#[test]
fn table_sort_resumes_owned_comparators_and_preserves_pending_state() {
    let source = make_source(
        b"local wrapped=coroutine.wrap(function() local values={'b','a'} table.sort(values,function(left,right) local resume=coroutine.yield(left) collectgarbage('collect') return resume and left<right end) return values[1],values[2] end) local yielded=wrapped() local first,second=wrapped(true) return yielded,first,second".to_vec(),
    );
    for profile in SemanticProfile::ALL {
        let compiled = OwnedCompiler::default()
            .compile(&source, profile, compiler_identity())
            .unwrap();
        assert_eq!(
            Vm::default().execute_blu_v1(compiled.into_validated_artifact(), BluLimits::default()),
            Ok(vec![
                Value::String(Arc::from(&b"a"[..])),
                Value::String(Arc::from(&b"a"[..])),
                Value::String(Arc::from(&b"b"[..])),
            ]),
            "{profile}"
        );
    }
}

#[test]
fn table_sort_resumes_owned_metamethod_ordering() {
    let source = make_source(
        b"local wrapped=coroutine.wrap(function() local mt={__lt=function(left,right) local resume=coroutine.yield(left[1]) collectgarbage('collect') return resume and left[1]<right[1] end} local values={setmetatable({2},mt),setmetatable({1},mt)} table.sort(values) return values[1][1],values[2][1] end) local yielded=wrapped() local first,second=wrapped(true) return yielded,first,second".to_vec(),
    );
    for profile in SemanticProfile::ALL {
        let compiled = OwnedCompiler::default()
            .compile(&source, profile, compiler_identity())
            .unwrap();
        let one = if matches!(
            profile,
            SemanticProfile::Blu
                | SemanticProfile::Lua53
                | SemanticProfile::Lua54
                | SemanticProfile::Lua55
        ) {
            Value::Integer(1)
        } else {
            Value::Number(1.0)
        };
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
        assert_eq!(
            Vm::default().execute_blu_v1(compiled.into_validated_artifact(), BluLimits::default()),
            Ok(vec![one.clone(), one, two]),
            "{profile}"
        );
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
                Err(RuntimeError::Type {
                    operation: "call",
                    expected: "function",
                    actual: "nil",
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
fn whitespace_escape_can_end_after_a_final_line_break() {
    let source = make_source(b"return '\\z  \n\t\x0c\x0b\n'".to_vec());
    for profile in [SemanticProfile::Lua54, SemanticProfile::Lua55] {
        let compiled = OwnedCompiler::default()
            .compile(&source, profile, compiler_identity())
            .unwrap_or_else(|error| panic!("{profile}: {error}"));
        assert_eq!(
            compiled.artifact().main().constants,
            [Constant::String(Vec::new())],
            "{profile}"
        );
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
            b"local function pair(value) if value == 0 then return 40, 2 end return pair(value - 1) end local function forward() return pair(10000) end local a, b, c = forward() return a, b, c"
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
fn final_call_arguments_forward_all_results() {
    for profile in SemanticProfile::ALL {
        let integer_profile = matches!(
            profile,
            SemanticProfile::Blu
                | SemanticProfile::Lua53
                | SemanticProfile::Lua54
                | SemanticProfile::Lua55
        );
        let number = |value| {
            if integer_profile {
                Value::Integer(value)
            } else {
                Value::Number(value as f64)
            }
        };
        for (bytes, expected) in [
            (
                b"local function pair() return 2, 3 end local function sink(...) return select('#', ...), ... end return sink(1, pair())"
                    .as_slice(),
                vec![number(3), number(1), number(2), number(3)],
            ),
            (
                b"local object={} function object:pair() return 2,3 end local function sink(...) return select('#',...),... end return sink(1,object:pair())"
                    .as_slice(),
                vec![number(3), number(1), number(2), number(3)],
            ),
            (
                b"local function pass(...) return ... end local function sink(...) return select('#',...),... end local function forward(...) return sink(1,pass(...)) end return forward(2,3)"
                    .as_slice(),
                vec![number(3), number(1), number(2), number(3)],
            ),
            (
                b"local function pair() return 2,3 end local function sink(...) return select('#',...),... end return sink(pair(),4)"
                    .as_slice(),
                vec![number(2), number(2), number(4)],
            ),
            (
                b"local function pair() return 2,3 end local function sink(...) return select('#',...),... end return 0,sink(1,pair())"
                    .as_slice(),
                vec![number(0), number(3), number(1), number(2), number(3)],
            ),
            (
                b"local function pair() return 2,3 end local function pass(...) return ... end local function sink(...) return select('#',...),... end return sink(1,pass(pair()))"
                    .as_slice(),
                vec![number(3), number(1), number(2), number(3)],
            ),
        ] {
            let source = make_source(bytes.to_vec());
            let compiled = OwnedCompiler::default()
                .compile(&source, profile, compiler_identity())
                .unwrap();
            assert_eq!(
                Vm::default()
                    .execute_blu_v1(compiled.into_validated_artifact(), BluLimits::default()),
                Ok(expected),
                "{profile}"
            );
        }
    }

    let source = make_source(
        b"local function sink(...) return select('#',...),... end return sink(1,native_pair())"
            .to_vec(),
    );
    let compiled = OwnedCompiler::default()
        .compile(&source, SemanticProfile::Blu, compiler_identity())
        .unwrap();
    let mut vm = Vm::default();
    let pair = vm.register_function(|_, _| Ok(vec![Value::Integer(2), Value::Integer(3)]));
    vm.set_global(b"native_pair".as_slice(), Value::NativeFunction(pair));
    assert_eq!(
        vm.execute_blu_v1(compiled.into_validated_artifact(), BluLimits::default()),
        Ok(vec![
            Value::Integer(3),
            Value::Integer(1),
            Value::Integer(2),
            Value::Integer(3),
        ])
    );

    let source = make_source(
        b"local function pair() return {value=2},{value=3} end local a,b,c=collect_args({},pair()) return a~=nil,b.value,c.value"
            .to_vec(),
    );
    let compiled = OwnedCompiler::default()
        .compile(&source, SemanticProfile::Blu, compiler_identity())
        .unwrap();
    let mut vm = Vm::default();
    let collect = vm.register_function(|vm, arguments| {
        vm.collect(core::iter::empty())?;
        Ok(arguments.to_vec())
    });
    vm.set_global(b"collect_args".as_slice(), Value::NativeFunction(collect));
    assert_eq!(
        vm.execute_blu_v1(compiled.into_validated_artifact(), BluLimits::default()),
        Ok(vec![
            Value::Boolean(true),
            Value::Integer(2),
            Value::Integer(3),
        ])
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
fn math_integer_bounds_are_profile_gated_and_exact() {
    let source = make_source(
        b"return math.mininteger,math.maxinteger,math.mininteger==nil,math.maxinteger==nil"
            .to_vec(),
    );
    for profile in SemanticProfile::ALL {
        let compiled = OwnedCompiler::default()
            .compile(&source, profile, compiler_identity())
            .unwrap();
        let expected = if matches!(
            profile,
            SemanticProfile::Blu
                | SemanticProfile::Lua53
                | SemanticProfile::Lua54
                | SemanticProfile::Lua55
        ) {
            vec![
                Value::Integer(i64::MIN),
                Value::Integer(i64::MAX),
                Value::Boolean(false),
                Value::Boolean(false),
            ]
        } else {
            vec![
                Value::Nil,
                Value::Nil,
                Value::Boolean(true),
                Value::Boolean(true),
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
            assert!(
                matches!(
                    result,
                    Err(blu_runtime::RuntimeError::Type {
                        operation: "math.atan",
                        ..
                    }) | Err(blu_runtime::RuntimeError::LuauMessage(_))
                ),
                "{profile}"
            );
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
        assert!(
            matches!(
                Vm::default()
                    .execute_blu_v1(compiled.into_validated_artifact(), BluLimits::default()),
                Err(blu_runtime::RuntimeError::Type {
                    operation: "math.acos",
                    ..
                }) | Err(blu_runtime::RuntimeError::LuauMessage(_))
            ),
            "{profile}"
        );
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
        let result =
            Vm::default().execute_blu_v1(compiled.into_validated_artifact(), BluLimits::default());
        if profile == SemanticProfile::Luau {
            assert!(matches!(result, Err(RuntimeError::LuauMessage(message)) if
                String::from_utf8_lossy(&message).contains("invalid argument #1 to 'modf'")));
        } else {
            assert!(matches!(
                result,
                Err(RuntimeError::Type {
                    operation: "math.modf",
                    ..
                })
            ));
        }
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
fn math_pow_follows_the_shared_numeric_contract() {
    let source = make_source(b"return math.pow(2,3),math.pow(-1,0.5)".to_vec());
    let invalid = make_source(b"return math.pow(2,'x')".to_vec());
    for profile in SemanticProfile::ALL {
        let compiled = OwnedCompiler::default()
            .compile(&source, profile, compiler_identity())
            .unwrap();
        let values =
            Vm::default().execute_blu_v1(compiled.into_validated_artifact(), BluLimits::default());
        assert!(
            matches!(values, Ok(values) if values[0] == Value::Number(8.0) && matches!(values[1], Value::Number(value) if value.is_nan())),
            "{profile}"
        );

        let compiled = OwnedCompiler::default()
            .compile(&invalid, profile, compiler_identity())
            .unwrap();
        assert!(
            matches!(
                Vm::default()
                    .execute_blu_v1(compiled.into_validated_artifact(), BluLimits::default()),
                Err(RuntimeError::Type {
                    operation: "math.pow",
                    ..
                }) | Err(RuntimeError::LuauMessage(_))
            ),
            "{profile}"
        );
    }
}

#[test]
fn math_frexp_and_ldexp_follow_profile_numeric_contracts() {
    let source = make_source(
        b"local f,e=math.frexp(-12) local z,ze=math.frexp(-0.0) local sf,se=math.frexp(5e-324) local inf,ie=math.frexp(math.huge) return f,e,math.ldexp(f,e),z,ze,sf,se,math.ldexp(sf,se),inf,ie"
            .to_vec(),
    );
    let fractional = make_source(b"return math.ldexp(0.5,2.5)".to_vec());
    for profile in SemanticProfile::ALL {
        let compiled = OwnedCompiler::default()
            .compile(&source, profile, compiler_identity())
            .unwrap();
        let exponent = if matches!(
            profile,
            SemanticProfile::Blu
                | SemanticProfile::Lua53
                | SemanticProfile::Lua54
                | SemanticProfile::Lua55
        ) {
            Value::Integer(4)
        } else {
            Value::Number(4.0)
        };
        let zero_exponent = if matches!(
            profile,
            SemanticProfile::Blu
                | SemanticProfile::Lua53
                | SemanticProfile::Lua54
                | SemanticProfile::Lua55
        ) {
            Value::Integer(0)
        } else {
            Value::Number(0.0)
        };
        let values =
            Vm::default().execute_blu_v1(compiled.into_validated_artifact(), BluLimits::default());
        assert!(
            matches!(
                values,
                Ok(values)
                    if values == [
                        Value::Number(-0.75),
                        exponent,
                        Value::Number(-12.0),
                        Value::Number(-0.0),
                        zero_exponent,
                        Value::Number(0.5),
                        if matches!(
                            profile,
                            SemanticProfile::Blu
                                | SemanticProfile::Lua53
                                | SemanticProfile::Lua54
                                | SemanticProfile::Lua55
                        ) {
                            Value::Integer(-1073)
                        } else {
                            Value::Number(-1073.0)
                        },
                        Value::Number(f64::from_bits(1)),
                        Value::Number(f64::INFINITY),
                        if matches!(
                            profile,
                            SemanticProfile::Blu
                                | SemanticProfile::Lua53
                                | SemanticProfile::Lua54
                                | SemanticProfile::Lua55
                        ) {
                            Value::Integer(0)
                        } else {
                            Value::Number(0.0)
                        },
                    ]
            ),
            "{profile}"
        );

        let compiled = OwnedCompiler::default()
            .compile(&fractional, profile, compiler_identity())
            .unwrap();
        let result =
            Vm::default().execute_blu_v1(compiled.into_validated_artifact(), BluLimits::default());
        if matches!(
            profile,
            SemanticProfile::Luau | SemanticProfile::Lua51 | SemanticProfile::Lua52
        ) {
            assert_eq!(result, Ok(vec![Value::Number(2.0)]), "{profile}");
        } else {
            assert!(matches!(
                result,
                Err(RuntimeError::Type {
                    operation: "math.ldexp",
                    ..
                })
            ));
        }
    }
}

#[test]
fn legacy_elementary_math_is_explicitly_removed_only_in_lua55() {
    let source = make_source(
        b"return math.sinh(0),math.cosh(0),math.tanh(0),math.log10(100),math.atan2(1,0)".to_vec(),
    );
    for profile in SemanticProfile::ALL {
        let compiled = OwnedCompiler::default()
            .compile(&source, profile, compiler_identity())
            .unwrap();
        let result =
            Vm::default().execute_blu_v1(compiled.into_validated_artifact(), BluLimits::default());
        if profile == SemanticProfile::Lua55 {
            assert!(matches!(
                result,
                Err(RuntimeError::Type {
                    operation: "call",
                    ..
                })
            ));
        } else {
            assert_eq!(
                result,
                Ok(vec![
                    Value::Number(0.0),
                    Value::Number(1.0),
                    Value::Number(0.0),
                    Value::Number(2.0),
                    Value::Number(core::f64::consts::FRAC_PI_2),
                ]),
                "{profile}"
            );
        }
    }
}

#[test]
fn math_random_and_randomseed_follow_profile_contracts() {
    for profile in SemanticProfile::ALL {
        let source = make_source(
            b"math.randomseed(123,456) return math.random(),math.random(1,1),math.random(-2,-2)"
                .to_vec(),
        );
        let compiled = OwnedCompiler::default()
            .compile(&source, profile, compiler_identity())
            .unwrap();
        let compiled_again = OwnedCompiler::default()
            .compile(&source, profile, compiler_identity())
            .unwrap();
        let first = Vm::default()
            .execute_blu_v1(compiled.into_validated_artifact(), BluLimits::default())
            .unwrap();
        let second = Vm::default()
            .execute_blu_v1(
                compiled_again.into_validated_artifact(),
                BluLimits::default(),
            )
            .unwrap();
        assert_eq!(first, second, "{profile}");
        assert!(matches!(first[0], Value::Number(value) if (0.0..1.0).contains(&value)));
        if matches!(
            profile,
            SemanticProfile::Blu
                | SemanticProfile::Lua53
                | SemanticProfile::Lua54
                | SemanticProfile::Lua55
        ) {
            assert_eq!(first[1..], [Value::Integer(1), Value::Integer(-2)]);
        } else {
            assert_eq!(first[1..], [Value::Number(1.0), Value::Number(-2.0)]);
        }

        let source = make_source(b"return math.randomseed(123,456)".to_vec());
        let compiled = OwnedCompiler::default()
            .compile(&source, profile, compiler_identity())
            .unwrap();
        let seeds = Vm::default()
            .execute_blu_v1(compiled.into_validated_artifact(), BluLimits::default())
            .unwrap();
        if matches!(
            profile,
            SemanticProfile::Blu | SemanticProfile::Lua54 | SemanticProfile::Lua55
        ) {
            assert_eq!(seeds, vec![Value::Integer(123), Value::Integer(456)]);
        } else {
            assert!(seeds.is_empty(), "{profile}");
        }

        let source = make_source(b"return math.random(0)".to_vec());
        let compiled = OwnedCompiler::default()
            .compile(&source, profile, compiler_identity())
            .unwrap();
        let result =
            Vm::default().execute_blu_v1(compiled.into_validated_artifact(), BluLimits::default());
        if matches!(
            profile,
            SemanticProfile::Blu | SemanticProfile::Lua54 | SemanticProfile::Lua55
        ) {
            assert!(
                matches!(result, Ok(values) if matches!(values.as_slice(), [Value::Integer(_)]))
            );
        } else {
            assert!(matches!(
                result,
                Err(blu_runtime::RuntimeError::InvalidRange {
                    operation: "math.random"
                })
            ));
        }

        let source = make_source(b"return math.randomseed()".to_vec());
        let compiled = OwnedCompiler::default()
            .compile(&source, profile, compiler_identity())
            .unwrap();
        let result =
            Vm::default().execute_blu_v1(compiled.into_validated_artifact(), BluLimits::default());
        if matches!(
            profile,
            SemanticProfile::Blu | SemanticProfile::Lua54 | SemanticProfile::Lua55
        ) {
            assert!(matches!(
                result,
                Ok(values)
                    if matches!(
                        values.as_slice(),
                        [Value::Integer(_), Value::Integer(_)]
                    )
            ));
        } else {
            assert!(matches!(
                result,
                Err(blu_runtime::RuntimeError::Argument {
                    function: "math.randomseed",
                    index: 1
                })
            ));
        }

        let source = make_source(b"return math.random(2.5,2.5)".to_vec());
        let compiled = OwnedCompiler::default()
            .compile(&source, profile, compiler_identity())
            .unwrap();
        let result =
            Vm::default().execute_blu_v1(compiled.into_validated_artifact(), BluLimits::default());
        match profile {
            SemanticProfile::Luau | SemanticProfile::Lua51 => {
                assert_eq!(result, Ok(vec![Value::Number(2.0)]));
            }
            SemanticProfile::Lua52 => {
                assert_eq!(result, Ok(vec![Value::Number(2.5)]));
            }
            _ => assert!(matches!(
                result,
                Err(blu_runtime::RuntimeError::Type {
                    operation: "math.random",
                    ..
                })
            )),
        }

        let fractional_seed = make_source(b"math.randomseed(2.5) return math.random()".to_vec());
        let integral_seed = make_source(b"math.randomseed(2) return math.random()".to_vec());
        let fractional = OwnedCompiler::default()
            .compile(&fractional_seed, profile, compiler_identity())
            .unwrap();
        let integral = OwnedCompiler::default()
            .compile(&integral_seed, profile, compiler_identity())
            .unwrap();
        let fractional = Vm::default()
            .execute_blu_v1(fractional.into_validated_artifact(), BluLimits::default());
        if matches!(
            profile,
            SemanticProfile::Blu | SemanticProfile::Lua54 | SemanticProfile::Lua55
        ) {
            assert!(matches!(
                fractional,
                Err(blu_runtime::RuntimeError::Type {
                    operation: "math.randomseed",
                    ..
                })
            ));
        } else {
            assert_eq!(
                fractional,
                Vm::default()
                    .execute_blu_v1(integral.into_validated_artifact(), BluLimits::default())
            );
        }

        let source = make_source(b"return math.random(1,2,3)".to_vec());
        let compiled = OwnedCompiler::default()
            .compile(&source, profile, compiler_identity())
            .unwrap();
        let result =
            Vm::default().execute_blu_v1(compiled.into_validated_artifact(), BluLimits::default());
        if profile == SemanticProfile::Luau {
            assert_eq!(
                result,
                Err(blu_runtime::RuntimeError::Raised(Value::String(Arc::from(
                    &b"wrong number of arguments"[..],
                ))))
            );
        } else {
            assert!(matches!(
                result,
                Err(blu_runtime::RuntimeError::ArgumentCount {
                    function: "math.random",
                    actual: 3,
                    ..
                })
            ));
        }
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
fn math_type_and_tointeger_follow_modern_profile_contracts() {
    let source = make_source(
        b"return math.type(math.floor(3)),math.type(3.5),math.type('3'),math.tointeger('3'),math.tointeger(3.2),math.tointeger(-9223372036854775808.0),math.tointeger(9223372036854775808.0),math.tointeger('0x1.8p1'),math.tointeger('0x1.1p1'),math.ult(-1,1),math.ult(1,-1),math.ult('1',2)"
            .to_vec(),
    );
    for profile in SemanticProfile::ALL {
        let compiled = OwnedCompiler::default()
            .compile(&source, profile, compiler_identity())
            .unwrap();
        let result =
            Vm::default().execute_blu_v1(compiled.into_validated_artifact(), BluLimits::default());
        if matches!(
            profile,
            SemanticProfile::Blu
                | SemanticProfile::Lua53
                | SemanticProfile::Lua54
                | SemanticProfile::Lua55
        ) {
            assert_eq!(
                result,
                Ok(vec![
                    Value::String(Arc::from(&b"integer"[..])),
                    Value::String(Arc::from(&b"float"[..])),
                    Value::Nil,
                    Value::Integer(3),
                    Value::Nil,
                    Value::Integer(i64::MIN),
                    Value::Nil,
                    Value::Integer(3),
                    Value::Nil,
                    Value::Boolean(false),
                    Value::Boolean(true),
                    Value::Boolean(true),
                ]),
                "{profile}"
            );
        } else {
            assert!(
                matches!(
                    result,
                    Err(RuntimeError::Type {
                        operation: "call",
                        ..
                    })
                ),
                "{profile}"
            );
            let source = make_source(b"return math.tointeger(3)".to_vec());
            let compiled = OwnedCompiler::default()
                .compile(&source, profile, compiler_identity())
                .unwrap();
            let result = Vm::default()
                .execute_blu_v1(compiled.into_validated_artifact(), BluLimits::default());
            assert!(
                matches!(
                    result,
                    Err(RuntimeError::Type {
                        operation: "call",
                        ..
                    })
                ),
                "{profile}"
            );
        }

        let source = make_source(b"return math.ult(1.5,2)".to_vec());
        let compiled = OwnedCompiler::default()
            .compile(&source, profile, compiler_identity())
            .unwrap();
        let result =
            Vm::default().execute_blu_v1(compiled.into_validated_artifact(), BluLimits::default());
        if matches!(
            profile,
            SemanticProfile::Blu
                | SemanticProfile::Lua53
                | SemanticProfile::Lua54
                | SemanticProfile::Lua55
        ) {
            assert_eq!(
                result,
                Err(RuntimeError::Type {
                    operation: "math.ult",
                    expected: "integer",
                    actual: "number",
                }),
                "{profile}"
            );
        } else {
            assert!(
                matches!(
                    result,
                    Err(RuntimeError::Type {
                        operation: "call",
                        ..
                    })
                ),
                "{profile}"
            );
        }
    }
}

#[test]
fn bit32_core_follows_profile_specific_conversion_and_result_rules() {
    let source = make_source(
        b"return bit32.band(),bit32.bor(),bit32.bxor(),bit32.band(0xffffffff,0x12345678),bit32.bnot(0),bit32.lshift(1,-1),bit32.rshift(1,-1),bit32.lshift(1,32),bit32.arshift(0x80000000,1),bit32.band('3',1),bit32.band(-1,0xffffffff),bit32.lrotate(0x12345678,8),bit32.rrotate(0x12345678,8),bit32.lrotate(1,-1),bit32.extract(0xabcdef01,8,8),bit32.extract(0xabcdef01,0,32),bit32.replace(0xabcdef01,0x12,8,8)"
            .to_vec(),
    );
    for profile in SemanticProfile::ALL {
        let compiled = OwnedCompiler::default()
            .compile(&source, profile, compiler_identity())
            .unwrap();
        let result =
            Vm::default().execute_blu_v1(compiled.into_validated_artifact(), BluLimits::default());
        if matches!(
            profile,
            SemanticProfile::Blu
                | SemanticProfile::Luau
                | SemanticProfile::Lua52
                | SemanticProfile::Lua53
        ) {
            let integral = |value| {
                if matches!(profile, SemanticProfile::Blu | SemanticProfile::Lua53) {
                    Value::Integer(value)
                } else {
                    Value::Number(value as f64)
                }
            };
            assert_eq!(
                result,
                Ok(vec![
                    integral(4_294_967_295),
                    integral(0),
                    integral(0),
                    integral(0x1234_5678),
                    integral(4_294_967_295),
                    integral(0),
                    integral(2),
                    integral(0),
                    integral(0xc000_0000),
                    integral(1),
                    integral(4_294_967_295),
                    integral(0x3456_7812),
                    integral(0x7812_3456),
                    integral(0x8000_0000),
                    integral(0xef),
                    integral(0xabcd_ef01),
                    integral(0xabcd_1201),
                ]),
                "{profile}"
            );
        } else {
            assert!(
                matches!(
                    result,
                    Err(RuntimeError::Type {
                        operation: "table index",
                        ..
                    })
                ),
                "{profile}"
            );
        }

        let fractional = make_source(b"return bit32.band(1.5,3)".to_vec());
        let compiled = OwnedCompiler::default()
            .compile(&fractional, profile, compiler_identity())
            .unwrap();
        let result =
            Vm::default().execute_blu_v1(compiled.into_validated_artifact(), BluLimits::default());
        match profile {
            SemanticProfile::Blu => assert_eq!(result, Ok(vec![Value::Integer(1)])),
            SemanticProfile::Luau => assert_eq!(result, Ok(vec![Value::Number(1.0)])),
            SemanticProfile::Lua52 => assert_eq!(result, Ok(vec![Value::Number(2.0)])),
            SemanticProfile::Lua53 => assert_eq!(
                result,
                Err(RuntimeError::Type {
                    operation: "bit32.band",
                    expected: "integer-representable number",
                    actual: "number",
                })
            ),
            SemanticProfile::Lua51 | SemanticProfile::Lua54 | SemanticProfile::Lua55 => {
                assert!(matches!(
                    result,
                    Err(RuntimeError::Type {
                        operation: "table index",
                        ..
                    })
                ));
            }
            _ => panic!("unhandled semantic profile {profile}"),
        }

        let invalid = make_source(b"return bit32.bnot('nope')".to_vec());
        let compiled = OwnedCompiler::default()
            .compile(&invalid, profile, compiler_identity())
            .unwrap();
        let result =
            Vm::default().execute_blu_v1(compiled.into_validated_artifact(), BluLimits::default());
        if matches!(
            profile,
            SemanticProfile::Blu
                | SemanticProfile::Luau
                | SemanticProfile::Lua52
                | SemanticProfile::Lua53
        ) {
            assert_eq!(
                result,
                Err(RuntimeError::Type {
                    operation: "bit32.bnot",
                    expected: "number",
                    actual: "string",
                }),
                "{profile}"
            );
        } else {
            assert!(matches!(
                result,
                Err(RuntimeError::Type {
                    operation: "table index",
                    expected: "table",
                    actual: "nil",
                })
            ));
        }

        let invalid_range = make_source(b"return bit32.extract(1,31,2)".to_vec());
        let compiled = OwnedCompiler::default()
            .compile(&invalid_range, profile, compiler_identity())
            .unwrap();
        let result =
            Vm::default().execute_blu_v1(compiled.into_validated_artifact(), BluLimits::default());
        if matches!(
            profile,
            SemanticProfile::Blu
                | SemanticProfile::Luau
                | SemanticProfile::Lua52
                | SemanticProfile::Lua53
        ) {
            assert_eq!(
                result,
                Err(RuntimeError::InvalidRange {
                    operation: "bit32.extract",
                }),
                "{profile}"
            );
        } else {
            assert!(matches!(
                result,
                Err(RuntimeError::Type {
                    operation: "table index",
                    ..
                })
            ));
        }
    }
}

#[test]
fn modern_bitwise_syntax_uses_64_bit_integer_semantics_and_profile_gates() {
    let source = make_source(
        b"return 0xf0&0x3c,0xf0|0x0f,0xaa~0xff,1<<63,-1>>1,1<<-1,~0,1|2~3&6<<1+1,3.0&1".to_vec(),
    );
    for profile in SemanticProfile::ALL {
        let compiled = OwnedCompiler::default().compile(&source, profile, compiler_identity());
        if matches!(
            profile,
            SemanticProfile::Blu
                | SemanticProfile::Lua53
                | SemanticProfile::Lua54
                | SemanticProfile::Lua55
        ) {
            let compiled = compiled.unwrap();
            assert_eq!(
                Vm::default()
                    .execute_blu_v1(compiled.into_validated_artifact(), BluLimits::default()),
                Ok(vec![
                    Value::Integer(0x30),
                    Value::Integer(0xff),
                    Value::Integer(0x55),
                    Value::Integer(i64::MIN),
                    Value::Integer(i64::MAX),
                    Value::Integer(0),
                    Value::Integer(-1),
                    Value::Integer(3),
                    Value::Integer(1),
                ]),
                "{profile}"
            );
        } else {
            assert!(compiled.is_err(), "{profile}: {compiled:?}");
        }
    }

    for (profile, expected) in [
        (SemanticProfile::Blu, false),
        (SemanticProfile::Lua53, true),
        (SemanticProfile::Lua54, false),
        (SemanticProfile::Lua55, false),
    ] {
        let string_source = make_source(b"return '3'&1".to_vec());
        let compiled = OwnedCompiler::default()
            .compile(&string_source, profile, compiler_identity())
            .unwrap();
        let result =
            Vm::default().execute_blu_v1(compiled.into_validated_artifact(), BluLimits::default());
        if expected {
            assert_eq!(result, Ok(vec![Value::Integer(1)]), "{profile}");
        } else {
            assert_eq!(
                result,
                Err(RuntimeError::Type {
                    operation: "bitwise operation",
                    expected: "integer-representable number",
                    actual: "string",
                }),
                "{profile}"
            );
        }
    }
}

#[test]
fn compound_assignments_evaluate_targets_once_and_use_luau_operators() {
    let source = make_source(
        b"local a=5 a+=3 local b=5 b-=3 local c=5 c*=3 local d=5 d/=2 local e=5 e%=3 local f=2 f^=3 local g='a' g..='b' local calls=0 local t={[1]=10} local function target() calls+=1 return t end local function key() calls+=1 return 1 end target()[key()]+=(function() calls+=1 return 5 end)() return a,b,c,d,e,f,g,t[1],calls"
            .to_vec(),
    );
    for profile in SemanticProfile::ALL {
        let compiled = OwnedCompiler::default().compile(&source, profile, compiler_identity());
        if matches!(profile, SemanticProfile::Blu | SemanticProfile::Luau) {
            let integral = |value| {
                if profile == SemanticProfile::Blu {
                    Value::Integer(value)
                } else {
                    Value::Number(value as f64)
                }
            };
            assert_eq!(
                Vm::default().execute_blu_v1(
                    compiled.unwrap().into_validated_artifact(),
                    BluLimits::default()
                ),
                Ok(vec![
                    integral(8),
                    integral(2),
                    integral(15),
                    Value::Number(2.5),
                    integral(2),
                    Value::Number(8.0),
                    Value::String(Arc::from(&b"ab"[..])),
                    integral(15),
                    integral(3),
                ]),
                "{profile}"
            );
        } else {
            assert!(compiled.is_err(), "{profile}: {compiled:?}");
        }
    }

    let floor = make_source(b"local x=5 x//=2 return x".to_vec());
    for (profile, expected) in [
        (SemanticProfile::Blu, Value::Integer(2)),
        (SemanticProfile::Luau, Value::Number(2.0)),
    ] {
        let compiled = OwnedCompiler::default()
            .compile(&floor, profile, compiler_identity())
            .unwrap();
        assert_eq!(
            Vm::default().execute_blu_v1(compiled.into_validated_artifact(), BluLimits::default()),
            Ok(vec![expected]),
            "{profile}"
        );
    }

    let snapshot = make_source(
        b"local x=10 local function rhs() x=100 return 5 end x+=rhs() return x".to_vec(),
    );
    for profile in [SemanticProfile::Blu, SemanticProfile::Luau] {
        let compiled = OwnedCompiler::default()
            .compile(&snapshot, profile, compiler_identity())
            .unwrap();
        assert_eq!(
            Vm::default().execute_blu_v1(compiled.into_validated_artifact(), BluLimits::default()),
            Ok(vec![if profile == SemanticProfile::Blu {
                Value::Integer(15)
            } else {
                Value::Number(15.0)
            }]),
            "{profile}"
        );
    }
}

#[test]
fn luau_math_extensions_are_profile_gated_and_edge_compatible() {
    let source = make_source(
        b"return math.clamp(5,1,3),math.clamp(-1,1,3),math.sign(-3),math.sign(0),math.sign(0/0),math.round(1.5),math.round(-1.5),math.round(1.49)"
            .to_vec(),
    );
    for profile in SemanticProfile::ALL {
        let compiled = OwnedCompiler::default()
            .compile(&source, profile, compiler_identity())
            .unwrap();
        let result =
            Vm::default().execute_blu_v1(compiled.into_validated_artifact(), BluLimits::default());
        if matches!(profile, SemanticProfile::Blu | SemanticProfile::Luau) {
            assert_eq!(
                result,
                Ok(vec![
                    Value::Number(3.0),
                    Value::Number(1.0),
                    Value::Number(-1.0),
                    Value::Number(0.0),
                    Value::Number(0.0),
                    Value::Number(2.0),
                    Value::Number(-2.0),
                    Value::Number(1.0),
                ]),
                "{profile}"
            );
        } else {
            assert!(matches!(
                result,
                Err(RuntimeError::Type {
                    operation: "call",
                    ..
                })
            ));
        }

        let source = make_source(b"return math.clamp(2,3,1)".to_vec());
        let compiled = OwnedCompiler::default()
            .compile(&source, profile, compiler_identity())
            .unwrap();
        let result =
            Vm::default().execute_blu_v1(compiled.into_validated_artifact(), BluLimits::default());
        if matches!(profile, SemanticProfile::Blu | SemanticProfile::Luau) {
            assert_eq!(
                result,
                Err(RuntimeError::InvalidRange {
                    operation: "math.clamp",
                }),
                "{profile}"
            );
        } else {
            assert!(matches!(
                result,
                Err(RuntimeError::Type {
                    operation: "call",
                    ..
                })
            ));
        }
    }
}

#[test]
fn luau_math_classification_and_interpolation_extensions_are_profile_gated() {
    let source = make_source(
        b"local nan=0/0 return math.isnan(nan),math.isinf(math.huge),math.isfinite(1),math.isfinite(math.huge),math.lerp(10,20,0.25),math.lerp(math.huge,-math.huge,1),math.map(5,0,10,0,100)"
            .to_vec(),
    );
    for profile in SemanticProfile::ALL {
        let compiled = OwnedCompiler::default()
            .compile(&source, profile, compiler_identity())
            .unwrap();
        let result =
            Vm::default().execute_blu_v1(compiled.into_validated_artifact(), BluLimits::default());
        if matches!(profile, SemanticProfile::Blu | SemanticProfile::Luau) {
            assert_eq!(
                result,
                Ok(vec![
                    Value::Boolean(true),
                    Value::Boolean(true),
                    Value::Boolean(true),
                    Value::Boolean(false),
                    Value::Number(12.5),
                    Value::Number(f64::NEG_INFINITY),
                    Value::Number(50.0),
                ]),
                "{profile}"
            );
        } else {
            assert!(matches!(
                result,
                Err(RuntimeError::Type {
                    operation: "call",
                    ..
                })
            ));
        }
    }
}

#[test]
fn luau_math_noise_matches_the_pinned_f32_perlin_contract() {
    let source = make_source(
        b"return math.noise(0.5),math.noise(0.5,0.5),math.noise(0.5,0.5,-0.5),math.noise(455.7204209769105,340.80410508750134,121.80087666537628),math.noise(0/0)"
            .to_vec(),
    );
    for profile in SemanticProfile::ALL {
        let compiled = OwnedCompiler::default()
            .compile(&source, profile, compiler_identity())
            .unwrap();
        let result =
            Vm::default().execute_blu_v1(compiled.into_validated_artifact(), BluLimits::default());
        if matches!(profile, SemanticProfile::Blu | SemanticProfile::Luau) {
            assert!(
                matches!(
                    result,
                    Ok(values)
                        if values[..4]
                            == [
                                Value::Number(0.0),
                                Value::Number(-0.25),
                                Value::Number(0.125),
                                Value::Number(0.501_070_976_257_324_2),
                            ]
                            && matches!(values[4], Value::Number(value) if value.is_nan())
                ),
                "{profile}"
            );
        } else {
            assert!(matches!(
                result,
                Err(RuntimeError::Type {
                    operation: "call",
                    ..
                })
            ));
        }
    }
}

#[test]
fn string_split_matches_luau_byte_and_empty_field_semantics() {
    let source = make_source(
        b"local a=string.split('a,b,,c',',') local b=string.split('a,b') local c=string.split('ab','') local d=string.split('','') local e=string.split('a--b--','--') local f=string.split(123,'2') return #a,a[1],a[2],a[3],a[4],b[1],b[2],c[1],c[2],#d,d[1],e[1],e[2],e[3],f[1],f[2]"
            .to_vec(),
    );
    for profile in SemanticProfile::ALL {
        let compiled = OwnedCompiler::default()
            .compile(&source, profile, compiler_identity())
            .unwrap();
        let result =
            Vm::default().execute_blu_v1(compiled.into_validated_artifact(), BluLimits::default());
        if matches!(profile, SemanticProfile::Blu | SemanticProfile::Luau) {
            let length = |value| {
                if profile == SemanticProfile::Blu {
                    Value::Integer(value)
                } else {
                    Value::Number(value as f64)
                }
            };
            assert_eq!(
                result,
                Ok(vec![
                    length(4),
                    Value::String(Arc::from(&b"a"[..])),
                    Value::String(Arc::from(&b"b"[..])),
                    Value::String(Arc::from(&b""[..])),
                    Value::String(Arc::from(&b"c"[..])),
                    Value::String(Arc::from(&b"a"[..])),
                    Value::String(Arc::from(&b"b"[..])),
                    Value::String(Arc::from(&b"a"[..])),
                    Value::String(Arc::from(&b"b"[..])),
                    length(0),
                    Value::Nil,
                    Value::String(Arc::from(&b"a"[..])),
                    Value::String(Arc::from(&b"b"[..])),
                    Value::String(Arc::from(&b""[..])),
                    Value::String(Arc::from(&b"1"[..])),
                    Value::String(Arc::from(&b"3"[..])),
                ]),
                "{profile}"
            );
        } else {
            assert!(matches!(
                result,
                Err(RuntimeError::Type {
                    operation: "call",
                    ..
                })
            ));
        }
    }
}

#[test]
fn string_format_executes_the_profile_common_conversion_core() {
    let source = make_source(
        b"return string.format('%s:%d:%i:%u:%x:%X:%o:%c:%f:%e:%E:%.3s:%.2f:%.1e:%.3E:%5s:%4d:%8.2f:%-5s:%-4d:%-8.2f:%%','ok',12,-2,-1,255,255,9,65,1.25,1.25,0.00125,'abcdef',1.25,1.25,0.00125,'xy',7,1.25,'xy',7,1.25)"
            .to_vec(),
    );
    let flags = make_source(
        b"return string.format('%+d|% d|%#x|%#X|%#o|%05d|%08.2f|%-05d|%#.5g',15,15,255,255,9,15,1.25,15,1.25)"
            .to_vec(),
    );
    let integer_precision = make_source(
        b"return string.format('%.3d|%+.3d|%.5u|%.5x|%#.5o|%.0d|%#.0o|%08.5d|%08.5x',12,-12,12,255,9,0,0,12,255)"
            .to_vec(),
    );
    let fractional = make_source(b"return string.format('%d',12.9)".to_vec());
    let unsupported = make_source(b"return string.format('%*d',7)".to_vec());
    let wide = make_source(b"return string.format('%100s','x')".to_vec());
    let dangling_width = make_source(b"return string.format('%12','x')".to_vec());
    for profile in SemanticProfile::ALL {
        let compile = |source: &SourceFile| {
            OwnedCompiler::default()
                .compile(source, profile, compiler_identity())
                .unwrap()
        };
        assert_eq!(
            Vm::default().execute_blu_v1(
                compile(&source).into_validated_artifact(),
                BluLimits::default(),
            ),
            Ok(vec![Value::String(Arc::from(
                &b"ok:12:-2:18446744073709551615:ff:FF:11:A:1.250000:1.250000e+00:1.250000E-03:abc:1.25:1.2e+00:1.250E-03:   xy:   7:    1.25:xy   :7   :1.25    :%"[..]
            ))]),
            "{profile}"
        );
        assert_eq!(
            Vm::default().execute_blu_v1(
                compile(&flags).into_validated_artifact(),
                BluLimits::default(),
            ),
            Ok(vec![Value::String(Arc::from(
                &b"+15| 15|0xff|0XFF|011|00015|00001.25|15   |1.2500"[..],
            ))]),
            "{profile}"
        );
        assert_eq!(
            Vm::default().execute_blu_v1(
                compile(&integer_precision).into_validated_artifact(),
                BluLimits::default(),
            ),
            Ok(vec![Value::String(Arc::from(
                &b"012|-012|00012|000ff|00011||0|   00012|   000ff"[..],
            ))]),
            "{profile}"
        );

        let fractional_result = Vm::default().execute_blu_v1(
            compile(&fractional).into_validated_artifact(),
            BluLimits::default(),
        );
        if matches!(
            profile,
            SemanticProfile::Luau | SemanticProfile::Lua51 | SemanticProfile::Lua52
        ) {
            assert_eq!(
                fractional_result,
                Ok(vec![Value::String(Arc::from(&b"12"[..]))]),
                "{profile}"
            );
        } else {
            assert_eq!(
                fractional_result,
                Err(RuntimeError::Type {
                    operation: "string.format",
                    expected: "integer",
                    actual: "number",
                }),
                "{profile}"
            );
        }

        assert_eq!(
            Vm::default().execute_blu_v1(
                compile(&unsupported).into_validated_artifact(),
                BluLimits::default(),
            ),
            Err(RuntimeError::UnsupportedLibraryFeature {
                function: "string.format",
                feature: if matches!(profile, SemanticProfile::Blu | SemanticProfile::Luau) {
                    "dynamic width or precision"
                } else {
                    "this conversion specifier"
                },
            }),
            "{profile}"
        );
        assert_eq!(
            Vm::default().execute_blu_v1(
                compile(&wide).into_validated_artifact(),
                BluLimits::default(),
            ),
            Err(RuntimeError::UnsupportedLibraryFeature {
                function: "string.format",
                feature: "invalid conversion",
            }),
            "{profile}"
        );
        assert_eq!(
            Vm::default().execute_blu_v1(
                compile(&dangling_width).into_validated_artifact(),
                BluLimits::default(),
            ),
            Err(RuntimeError::UnsupportedLibraryFeature {
                function: "string.format",
                feature: "a field width without a conversion specifier",
            }),
            "{profile}"
        );
    }
}

#[test]
fn string_format_modifier_rejections_follow_lua54_profiles() {
    let source = make_source(
        b"local function accepts(format, value) local ok = pcall(string.format, format, value) return ok end
return accepts('%#d', 15), accepts('%+u', 15), accepts('%#s', 15), accepts('%0s', 15), accepts('%+q', 15), accepts('%5q', 15), accepts('%05c', 65), accepts('%+x', 15)"
            .to_vec(),
    );
    for profile in SemanticProfile::ALL {
        let compiled = OwnedCompiler::default()
            .compile(&source, profile, compiler_identity())
            .unwrap();
        let accepted = !matches!(profile, SemanticProfile::Lua54 | SemanticProfile::Lua55);
        assert_eq!(
            Vm::default().execute_blu_v1(compiled.into_validated_artifact(), BluLimits::default()),
            Ok(vec![Value::Boolean(accepted); 8]),
            "{profile}"
        );
    }
}

#[test]
fn main_closure_preserves_modern_default_environment_and_format_profile() {
    let source = make_source(
        b"local function accepts(format, value) local ok, error = pcall(string.format, format, value) return ok, error end
local ok, error = accepts('%#d', 15)
return _VERSION, ok, error"
            .to_vec(),
    );
    let compiled = OwnedCompiler::default()
        .compile(&source, SemanticProfile::Lua52, compiler_identity())
        .unwrap();
    assert_eq!(
        Vm::default().execute_blu_v1(compiled.into_validated_artifact(), BluLimits::default()),
        Ok(vec![
            Value::String(Arc::from(&b"Lua 5.2"[..])),
            Value::Boolean(true),
            Value::String(Arc::from(&b"15"[..])),
        ])
    );
}

#[test]
fn string_format_supports_quoted_values_and_profile_hex_floats() {
    let quoted = make_source(b"return string.format('%q|%q|%q','a\\\"b\\\\c',12,12.5)".to_vec());
    let hexadecimal = make_source(
        b"return string.format('%.0a|%.1a|%.2a|%.3a|%.13a|%.3E',12.5,12.5,12.5,12.5,12.5,0.00125)"
            .to_vec(),
    );
    for profile in SemanticProfile::ALL {
        let compile = |source: &SourceFile| {
            OwnedCompiler::default()
                .compile(source, profile, compiler_identity())
                .unwrap()
        };
        let quoted_result = Vm::default().execute_blu_v1(
            compile(&quoted).into_validated_artifact(),
            BluLimits::default(),
        );
        let modern = matches!(
            profile,
            SemanticProfile::Blu
                | SemanticProfile::Lua53
                | SemanticProfile::Lua54
                | SemanticProfile::Lua55
        );
        let mut expected = if modern {
            b"\"a\\\"b\\\\c\"|12|".to_vec()
        } else {
            b"\"a\\\"b\\\\c\"|\"12\"|\"".to_vec()
        };
        let tail: &[u8] = if modern { b"0x1.9p+3" } else { b"12.5" };
        expected.extend_from_slice(tail);
        if !modern {
            expected.push(b'"');
        }
        assert_eq!(
            quoted_result,
            Ok(vec![Value::String(Arc::from(expected))]),
            "{profile}"
        );

        let hexadecimal_result = Vm::default().execute_blu_v1(
            compile(&hexadecimal).into_validated_artifact(),
            BluLimits::default(),
        );
        if matches!(
            profile,
            SemanticProfile::Blu
                | SemanticProfile::Lua53
                | SemanticProfile::Lua54
                | SemanticProfile::Lua55
        ) {
            assert_eq!(
                hexadecimal_result,
                Ok(vec![Value::String(Arc::from(
                    &b"0x2p+3|0x1.9p+3|0x1.90p+3|0x1.900p+3|0x1.9000000000000p+3|1.250E-03"[..],
                ))]),
                "{profile}"
            );
        } else {
            assert_eq!(
                hexadecimal_result,
                Err(RuntimeError::UnsupportedLibraryFeature {
                    function: "string.format",
                    feature: "this conversion specifier",
                }),
                "{profile}"
            );
        }
    }
}

#[test]
fn luau_table_create_and_find_are_bounded_and_profile_gated() {
    let source = make_source(
        b"local filled=table.create(3,'x') local empty=table.create(3) local fractional=table.create(1.5,'y') return #filled,filled[1],filled[3],filled[4],#empty,empty[1],#fractional,table.find({1,2,1},1),table.find({1,2,1},1,2),table.find({[3]='x'},'x')"
            .to_vec(),
    );
    for profile in SemanticProfile::ALL {
        let compiled = OwnedCompiler::default()
            .compile(&source, profile, compiler_identity())
            .unwrap();
        let result =
            Vm::default().execute_blu_v1(compiled.into_validated_artifact(), BluLimits::default());
        if matches!(profile, SemanticProfile::Blu | SemanticProfile::Lua55) {
            assert!(
                matches!(
                    result,
                    Err(RuntimeError::Type {
                        operation: "table.create",
                        expected: "integer",
                        ..
                    })
                ),
                "{profile}"
            );
            continue;
        }
        if profile == SemanticProfile::Luau {
            let integral = |value| {
                if profile == SemanticProfile::Blu {
                    Value::Integer(value)
                } else {
                    Value::Number(value as f64)
                }
            };
            assert_eq!(
                result,
                Ok(vec![
                    integral(3),
                    Value::String(Arc::from(&b"x"[..])),
                    Value::String(Arc::from(&b"x"[..])),
                    Value::Nil,
                    integral(0),
                    Value::Nil,
                    integral(1),
                    integral(1),
                    integral(3),
                    Value::Nil,
                ]),
                "{profile}"
            );
            for (source, operation) in [
                (b"return table.create(-1)".as_slice(), "table.create"),
                (b"return table.find({1},1,0)".as_slice(), "table.find"),
            ] {
                let compiled = OwnedCompiler::default()
                    .compile(&make_source(source.to_vec()), profile, compiler_identity())
                    .unwrap();
                assert_eq!(
                    Vm::default()
                        .execute_blu_v1(compiled.into_validated_artifact(), BluLimits::default()),
                    Err(RuntimeError::InvalidRange { operation }),
                    "{profile}"
                );
            }
        } else {
            assert!(matches!(
                result,
                Err(RuntimeError::Type {
                    operation: "call",
                    ..
                })
            ));
        }
    }
}

#[test]
fn luau_table_clear_and_clone_preserve_shallow_structure() {
    let source = make_source(
        b"local original={1,2,a=3} original.self=original local metatable={tag=1} setmetatable(original,metatable) local cloned=table.clone(original) local cleared=table.clear(original) return cleared,#original,original[1],original.a,#cloned,cloned[1],cloned[2],cloned.a,cloned.self==original,getmetatable(cloned)==metatable,cloned==original"
            .to_vec(),
    );
    for profile in SemanticProfile::ALL {
        let compiled = OwnedCompiler::default()
            .compile(&source, profile, compiler_identity())
            .unwrap();
        let result =
            Vm::default().execute_blu_v1(compiled.into_validated_artifact(), BluLimits::default());
        if matches!(profile, SemanticProfile::Blu | SemanticProfile::Luau) {
            let integral = |value| {
                if profile == SemanticProfile::Blu {
                    Value::Integer(value)
                } else {
                    Value::Number(value as f64)
                }
            };
            assert_eq!(
                result,
                Ok(vec![
                    Value::Nil,
                    integral(0),
                    Value::Nil,
                    Value::Nil,
                    integral(2),
                    Value::Number(1.0),
                    Value::Number(2.0),
                    Value::Number(3.0),
                    Value::Boolean(true),
                    Value::Boolean(true),
                    Value::Boolean(false),
                ]),
                "{profile}"
            );

            let source = make_source(
                b"local value=setmetatable({x=1},{__metatable='locked'}) return table.clone(value)"
                    .to_vec(),
            );
            let compiled = OwnedCompiler::default()
                .compile(&source, profile, compiler_identity())
                .unwrap();
            assert_eq!(
                Vm::default()
                    .execute_blu_v1(compiled.into_validated_artifact(), BluLimits::default()),
                Err(RuntimeError::MetatableProtected),
                "{profile}"
            );
        } else {
            assert!(matches!(
                result,
                Err(RuntimeError::Type {
                    operation: "call",
                    ..
                })
            ));
        }
    }
}

#[test]
fn legacy_table_size_helpers_follow_profile_availability() {
    let getn_source = make_source(b"return table.getn({10,20})".to_vec());
    let maxn_source =
        make_source(b"return table.maxn({[1]=true,[3]=true,[2.5]=true,[-9]=true})".to_vec());
    for profile in SemanticProfile::ALL {
        let compiled = OwnedCompiler::default()
            .compile(&getn_source, profile, compiler_identity())
            .unwrap();
        let result =
            Vm::default().execute_blu_v1(compiled.into_validated_artifact(), BluLimits::default());
        if matches!(
            profile,
            SemanticProfile::Blu | SemanticProfile::Luau | SemanticProfile::Lua51
        ) {
            assert_eq!(
                result,
                Ok(vec![if profile == SemanticProfile::Blu {
                    Value::Integer(2)
                } else {
                    Value::Number(2.0)
                }]),
                "{profile}"
            );
        } else {
            assert!(matches!(
                result,
                Err(RuntimeError::Type {
                    operation: "call",
                    ..
                })
            ));
        }

        let compiled = OwnedCompiler::default()
            .compile(&maxn_source, profile, compiler_identity())
            .unwrap();
        let result =
            Vm::default().execute_blu_v1(compiled.into_validated_artifact(), BluLimits::default());
        if matches!(
            profile,
            SemanticProfile::Blu
                | SemanticProfile::Luau
                | SemanticProfile::Lua51
                | SemanticProfile::Lua52
        ) {
            assert_eq!(result, Ok(vec![Value::Number(3.0)]), "{profile}");
        } else {
            assert!(matches!(
                result,
                Err(RuntimeError::Type {
                    operation: "call",
                    ..
                })
            ));
        }
    }
}

#[test]
fn legacy_table_foreach_callbacks_follow_profile_availability() {
    let source = make_source(
        b"local stop=table.foreach({a=1},function(key,value) return key..value end) local first,second,third=0,0,0.0 local result=table.foreachi({2,4,6},function(index,value) if index==1 then first=index+value elseif index==2 then second=index+value else third=index+value end if index==2 then return 'stop' end end) return stop,first,second,third,result".to_vec(),
    );
    for profile in SemanticProfile::ALL {
        let compiled = OwnedCompiler::default()
            .compile(&source, profile, compiler_identity())
            .unwrap();
        let result =
            Vm::default().execute_blu_v1(compiled.into_validated_artifact(), BluLimits::default());
        if matches!(
            profile,
            SemanticProfile::Blu | SemanticProfile::Luau | SemanticProfile::Lua51
        ) {
            assert_eq!(
                result,
                Ok(vec![
                    Value::String(Arc::from(&b"a1"[..])),
                    Value::Number(3.0),
                    Value::Number(6.0),
                    Value::Number(0.0),
                    Value::String(Arc::from(&b"stop"[..])),
                ])
            );
        } else {
            assert_eq!(
                result,
                Err(RuntimeError::Type {
                    operation: "call",
                    expected: "function",
                    actual: "nil",
                }),
                "{profile}"
            );
        }
    }
}

#[test]
fn lua51_table_foreach_callbacks_resume_owned_coroutines() {
    let source = make_source(
        b"local foreach=coroutine.wrap(function() local result=table.foreach({10,20},function(key,value) local replacement=coroutine.yield(value) collectgarbage('collect') return replacement end) return result end) local a=foreach() local b=foreach() local c=foreach('done') local foreachi=coroutine.wrap(function() local result=table.foreachi({30,40},function(index,value) local replacement=coroutine.yield(value) collectgarbage('collect') return replacement end) return result end) local d=foreachi() local e=foreachi() local f=foreachi('done') return a,b,c,d,e,f".to_vec(),
    );
    for profile in [
        SemanticProfile::Blu,
        SemanticProfile::Luau,
        SemanticProfile::Lua51,
    ] {
        let compiled = OwnedCompiler::default()
            .compile(&source, profile, compiler_identity())
            .unwrap();
        let result =
            Vm::default().execute_blu_v1(compiled.into_validated_artifact(), BluLimits::default());
        assert_eq!(
            result,
            Ok(vec![
                Value::Number(10.0),
                Value::Number(20.0),
                Value::String(Arc::from(&b"done"[..])),
                Value::Number(30.0),
                Value::Number(40.0),
                Value::String(Arc::from(&b"done"[..])),
            ]),
            "{profile}"
        );
    }
}

#[test]
fn yielding_legacy_library_callbacks_work_in_owned_return_calls() {
    let source = make_source(
        b"local wrapped=coroutine.wrap(function() return table.foreach({10,20},function(key,value) local replacement=coroutine.yield(value) return replacement end) end) local first=wrapped() local second=wrapped() local third=wrapped('done') return first,second,third".to_vec(),
    );
    for profile in [SemanticProfile::Blu, SemanticProfile::Lua51] {
        let compiled = OwnedCompiler::default()
            .compile(&source, profile, compiler_identity())
            .unwrap();
        assert_eq!(
            Vm::default().execute_blu_v1(compiled.into_validated_artifact(), BluLimits::default()),
            Ok(vec![
                Value::Number(10.0),
                Value::Number(20.0),
                Value::String(Arc::from(&b"done"[..])),
            ]),
            "{profile}"
        );
    }
}

#[test]
fn yielding_gsub_callbacks_work_in_owned_return_calls() {
    let source = make_source(
        b"local wrapped=coroutine.wrap(function() return string.gsub('aa','a',function(value) local replacement=coroutine.yield(value) return replacement end) end) local first=wrapped() local second=wrapped() local result,count=wrapped('X') return first,second,result,count".to_vec(),
    );
    for profile in SemanticProfile::ALL {
        let compiled = OwnedCompiler::default()
            .compile(&source, profile, compiler_identity())
            .unwrap();
        let count = if matches!(
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
        assert_eq!(
            Vm::default().execute_blu_v1(compiled.into_validated_artifact(), BluLimits::default()),
            Ok(vec![
                Value::String(Arc::from(&b"a"[..])),
                Value::String(Arc::from(&b"a"[..])),
                Value::String(Arc::from(&b"aX"[..])),
                count,
            ]),
            "{profile}"
        );
    }
}

#[test]
fn table_pack_and_unpack_names_follow_profile_availability() {
    let pack_source = make_source(b"local value=table.pack(10,nil,30) return value.n".to_vec());
    let table_unpack_source = make_source(
        b"local first,second=table.unpack({10,20,30},2,3) return first,second".to_vec(),
    );
    let global_unpack_source =
        make_source(b"local first,second=unpack({10,20,30},2,3) return first,second".to_vec());
    for profile in SemanticProfile::ALL {
        let modern = matches!(
            profile,
            SemanticProfile::Blu
                | SemanticProfile::Lua53
                | SemanticProfile::Lua54
                | SemanticProfile::Lua55
        );
        let count = if modern {
            Value::Integer(3)
        } else {
            Value::Number(3.0)
        };
        let value = |integer| {
            if modern {
                Value::Integer(integer)
            } else {
                Value::Number(integer as f64)
            }
        };

        let compiled = OwnedCompiler::default()
            .compile(&pack_source, profile, compiler_identity())
            .unwrap();
        let result =
            Vm::default().execute_blu_v1(compiled.into_validated_artifact(), BluLimits::default());
        if profile == SemanticProfile::Lua51 {
            assert_eq!(
                result,
                Err(RuntimeError::Type {
                    operation: "call",
                    expected: "function",
                    actual: "nil",
                })
            );
        } else {
            assert_eq!(result, Ok(vec![count]), "{profile}");
        }

        let compiled = OwnedCompiler::default()
            .compile(&table_unpack_source, profile, compiler_identity())
            .unwrap();
        let result =
            Vm::default().execute_blu_v1(compiled.into_validated_artifact(), BluLimits::default());
        if profile == SemanticProfile::Lua51 {
            assert_eq!(
                result,
                Err(RuntimeError::Type {
                    operation: "call",
                    expected: "function",
                    actual: "nil",
                })
            );
        } else {
            assert_eq!(result, Ok(vec![value(20), value(30)]), "{profile}");
        }

        let compiled = OwnedCompiler::default()
            .compile(&global_unpack_source, profile, compiler_identity())
            .unwrap();
        let result =
            Vm::default().execute_blu_v1(compiled.into_validated_artifact(), BluLimits::default());
        if matches!(
            profile,
            SemanticProfile::Blu
                | SemanticProfile::Luau
                | SemanticProfile::Lua51
                | SemanticProfile::Lua52
        ) {
            assert_eq!(result, Ok(vec![value(20), value(30)]), "{profile}");
        } else {
            assert_eq!(
                result,
                Err(RuntimeError::Type {
                    operation: "call",
                    expected: "function",
                    actual: "nil",
                })
            );
        }
    }
}

#[test]
fn luau_frozen_tables_enforce_heap_wide_immutability() {
    let source = make_source(
        b"local value={x=1} local frozen=table.freeze(value) local clone=table.clone(value) return frozen==value,table.isfrozen(value),table.isfrozen(clone),clone.x"
            .to_vec(),
    );
    for profile in SemanticProfile::ALL {
        let compiled = OwnedCompiler::default()
            .compile(&source, profile, compiler_identity())
            .unwrap();
        let result =
            Vm::default().execute_blu_v1(compiled.into_validated_artifact(), BluLimits::default());
        if matches!(profile, SemanticProfile::Blu | SemanticProfile::Luau) {
            assert_eq!(
                result,
                Ok(vec![
                    Value::Boolean(true),
                    Value::Boolean(true),
                    Value::Boolean(false),
                    Value::Number(1.0),
                ]),
                "{profile}"
            );
            for source in [
                b"local value=table.freeze({x=1}) value.x=2 return value".as_slice(),
                b"local value=table.freeze({x=1}) return rawset(value,'x',2)".as_slice(),
                b"local value=table.freeze({x=1}) return table.clear(value)".as_slice(),
                b"local value=table.freeze({x=1}) return setmetatable(value,{})".as_slice(),
                b"local value=table.freeze({2,1}) return table.sort(value)".as_slice(),
            ] {
                let compiled = OwnedCompiler::default()
                    .compile(&make_source(source.to_vec()), profile, compiler_identity())
                    .unwrap();
                assert_eq!(
                    Vm::default()
                        .execute_blu_v1(compiled.into_validated_artifact(), BluLimits::default()),
                    Err(RuntimeError::Heap(HeapError::FrozenTable)),
                    "{profile}"
                );
            }
            let source =
                make_source(b"local value=table.freeze({}) return table.freeze(value)".to_vec());
            let compiled = OwnedCompiler::default()
                .compile(&source, profile, compiler_identity())
                .unwrap();
            assert_eq!(
                Vm::default()
                    .execute_blu_v1(compiled.into_validated_artifact(), BluLimits::default()),
                Err(RuntimeError::Heap(HeapError::AlreadyFrozen)),
                "{profile}"
            );
            let source = make_source(
                b"local value=setmetatable({},{__metatable='locked'}) return table.freeze(value)"
                    .to_vec(),
            );
            let compiled = OwnedCompiler::default()
                .compile(&source, profile, compiler_identity())
                .unwrap();
            assert_eq!(
                Vm::default()
                    .execute_blu_v1(compiled.into_validated_artifact(), BluLimits::default()),
                Err(RuntimeError::MetatableProtected),
                "{profile}"
            );
        } else {
            assert_eq!(
                result,
                Err(RuntimeError::Type {
                    operation: "call",
                    expected: "function",
                    actual: "nil",
                }),
                "{profile}"
            );
        }
    }
}

#[test]
fn coroutine_introspection_uses_the_active_semantic_profile() {
    let main_source =
        make_source(b"local thread,main=coroutine.running() return type(thread),main".to_vec());
    let yieldable_source = make_source(b"return coroutine.isyieldable()".to_vec());
    for profile in SemanticProfile::ALL {
        let compile = |source: &SourceFile| {
            OwnedCompiler::default()
                .compile(source, profile, compiler_identity())
                .unwrap()
        };
        let main = Vm::default()
            .execute_blu_v1(
                compile(&main_source).into_validated_artifact(),
                BluLimits::default(),
            )
            .unwrap();
        assert_eq!(
            main,
            match profile {
                SemanticProfile::Lua51 => vec![Value::String(Arc::from(&b"nil"[..])), Value::Nil,],
                SemanticProfile::Luau => vec![Value::String(Arc::from(&b"nil"[..])), Value::Nil,],
                _ => vec![
                    Value::String(Arc::from(&b"thread"[..])),
                    Value::Boolean(true),
                ],
            },
            "{profile}"
        );

        let yieldable = Vm::default().execute_blu_v1(
            compile(&yieldable_source).into_validated_artifact(),
            BluLimits::default(),
        );
        match profile {
            SemanticProfile::Lua51 | SemanticProfile::Lua52 => assert_eq!(
                yieldable,
                Err(RuntimeError::Type {
                    operation: "call",
                    expected: "function",
                    actual: "nil",
                }),
                "{profile}"
            ),
            SemanticProfile::Luau => {
                assert_eq!(yieldable, Ok(vec![Value::Boolean(true)]), "{profile}");
            }
            _ => {
                assert_eq!(yieldable, Ok(vec![Value::Boolean(false)]), "{profile}");
            }
        }
    }
}

#[test]
fn tonumber_preserves_profile_subtypes_and_explicit_base_grammar() {
    let source = make_source(
        b"return tonumber(' 42 '),tonumber('ff',16),tonumber('0x10'),tonumber(3),tonumber('3.0',10),tonumber('nan'),tonumber('inf'),tonumber('ffffffffffffffff',16),tonumber('0x1.8p1'),tonumber('-0x1p2'),tonumber('x')"
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
        let integral = |value| {
            if modern {
                Value::Integer(value)
            } else {
                Value::Number(value as f64)
            }
        };
        let values = Vm::default()
            .execute_blu_v1(compiled.into_validated_artifact(), BluLimits::default())
            .unwrap();
        assert_eq!(
            &values[..4],
            &[integral(42), integral(255), integral(16), integral(3)]
        );
        let permissive = matches!(profile, SemanticProfile::Luau | SemanticProfile::Lua51);
        if permissive {
            assert_eq!(values[4], Value::Number(3.0), "{profile}");
            assert!(
                matches!(values[5], Value::Number(value) if value.is_nan()),
                "{profile}"
            );
            assert_eq!(values[6], Value::Number(f64::INFINITY), "{profile}");
        } else {
            assert_eq!(&values[4..7], &[Value::Nil, Value::Nil, Value::Nil]);
        }
        assert_eq!(
            values[7],
            if modern {
                Value::Integer(-1)
            } else {
                Value::Number(u64::MAX as f64)
            },
            "{profile}"
        );
        assert_eq!(values[8], Value::Number(3.0), "{profile}");
        assert_eq!(values[9], Value::Number(-4.0), "{profile}");
        assert_eq!(values[10], Value::Nil, "{profile}");
    }
}

#[test]
fn task_limit_collects_unreachable_coroutines_before_rejecting_growth() {
    let source = make_source(b"return coroutine.create(function() end)".to_vec());
    let compile = || {
        OwnedCompiler::default()
            .compile(&source, SemanticProfile::Blu, compiler_identity())
            .unwrap()
    };

    let mut blocked = Vm::default().with_task_limit(1);
    assert_eq!(blocked.task_limit(), 1);
    assert_eq!(
        blocked.execute_blu_v1(compile().into_validated_artifact(), BluLimits::default()),
        Err(RuntimeError::TaskLimit {
            required: 2,
            limit: 1,
        })
    );

    let mut vm = Vm::default().with_task_limit(2);
    let first = vm
        .execute_blu_v1(compile().into_validated_artifact(), BluLimits::default())
        .unwrap();
    assert!(matches!(first.as_slice(), [Value::Thread(_)]));
    assert_eq!(
        vm.execute_blu_v1(compile().into_validated_artifact(), BluLimits::default()),
        Err(RuntimeError::TaskLimit {
            required: 3,
            limit: 2,
        })
    );

    assert_eq!(vm.release_values(&first), 1);
    let replacement = vm
        .execute_blu_v1(compile().into_validated_artifact(), BluLimits::default())
        .unwrap();
    assert!(matches!(replacement.as_slice(), [Value::Thread(_)]));
}

#[test]
fn owned_coroutines_suspend_and_resume_blu_closures() {
    let source = make_source(
        b"local thread=coroutine.create(function(seed) local value=coroutine.yield(seed+1) return value+2 end) local first,yielded=coroutine.resume(thread,40) collectgarbage('collect') local second,result=coroutine.resume(thread,yielded) return first,yielded,second,result,coroutine.status(thread)".to_vec(),
    );
    for profile in SemanticProfile::ALL {
        let compiled = OwnedCompiler::default()
            .compile(&source, profile, compiler_identity())
            .unwrap();
        let result = Vm::default()
            .execute_blu_v1(compiled.into_validated_artifact(), BluLimits::default())
            .unwrap();
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
            vec![
                Value::Boolean(true),
                number(41),
                Value::Boolean(true),
                number(43),
                Value::String(Arc::from(&b"dead"[..])),
            ],
            "{profile}"
        );
    }
}

#[test]
fn owned_coroutine_wrap_forwards_yield_and_resume_values() {
    let source = make_source(
        b"local wrapped=coroutine.wrap(function() local value=coroutine.yield(7) return value+1 end) local yielded=wrapped() local result=wrapped(yielded) return yielded,result".to_vec(),
    );
    for profile in SemanticProfile::ALL {
        let compiled = OwnedCompiler::default()
            .compile(&source, profile, compiler_identity())
            .unwrap();
        let result = Vm::default()
            .execute_blu_v1(compiled.into_validated_artifact(), BluLimits::default())
            .unwrap();
        let seven = if matches!(
            profile,
            SemanticProfile::Blu
                | SemanticProfile::Lua53
                | SemanticProfile::Lua54
                | SemanticProfile::Lua55
        ) {
            Value::Integer(7)
        } else {
            Value::Number(7.0)
        };
        let one = if matches!(
            profile,
            SemanticProfile::Blu
                | SemanticProfile::Lua53
                | SemanticProfile::Lua54
                | SemanticProfile::Lua55
        ) {
            Value::Integer(8)
        } else {
            Value::Number(8.0)
        };
        assert_eq!(result, vec![seven, one], "{profile}");
    }
}

#[test]
fn legacy_gcinfo_is_profile_gated_and_reports_integer_kibibytes() {
    let source = make_source(b"return gcinfo()".to_vec());
    for profile in SemanticProfile::ALL {
        let compiled = OwnedCompiler::default()
            .compile(&source, profile, compiler_identity())
            .unwrap();
        let result =
            Vm::default().execute_blu_v1(compiled.into_validated_artifact(), BluLimits::default());
        match profile {
            SemanticProfile::Blu => {
                assert!(
                    matches!(result, Ok(values) if matches!(values.as_slice(), [Value::Integer(value)] if *value >= 0))
                );
            }
            SemanticProfile::Luau | SemanticProfile::Lua51 => {
                assert!(
                    matches!(result, Ok(values) if matches!(values.as_slice(), [Value::Number(value)] if *value >= 0.0 && value.fract() == 0.0))
                );
            }
            _ => assert!(
                matches!(
                    result,
                    Err(RuntimeError::Type {
                        operation: "call",
                        ..
                    })
                ),
                "{profile}"
            ),
        }
    }
}

#[test]
fn typeof_and_rawlen_follow_profile_availability() {
    let typeof_source = make_source(b"return typeof({})".to_vec());
    let rawlen_source = make_source(b"return rawlen('abc'),rawlen({10,20})".to_vec());
    for profile in SemanticProfile::ALL {
        let compiled = OwnedCompiler::default()
            .compile(&typeof_source, profile, compiler_identity())
            .unwrap();
        let result =
            Vm::default().execute_blu_v1(compiled.into_validated_artifact(), BluLimits::default());
        if matches!(profile, SemanticProfile::Blu | SemanticProfile::Luau) {
            assert_eq!(
                result,
                Ok(vec![Value::String(Arc::from(&b"table"[..]))]),
                "{profile}"
            );
        } else {
            assert_eq!(
                result,
                Err(RuntimeError::UnsupportedSemanticProfile {
                    operation: "typeof",
                    profile,
                }),
                "{profile}"
            );
        }

        let compiled = OwnedCompiler::default()
            .compile(&rawlen_source, profile, compiler_identity())
            .unwrap();
        let result =
            Vm::default().execute_blu_v1(compiled.into_validated_artifact(), BluLimits::default());
        if profile == SemanticProfile::Lua51 {
            assert_eq!(
                result,
                Err(RuntimeError::Type {
                    operation: "call",
                    expected: "function",
                    actual: "nil",
                })
            );
        } else {
            let modern = matches!(
                profile,
                SemanticProfile::Blu
                    | SemanticProfile::Lua53
                    | SemanticProfile::Lua54
                    | SemanticProfile::Lua55
            );
            let value = |integer| {
                if modern {
                    Value::Integer(integer)
                } else {
                    Value::Number(integer as f64)
                }
            };
            assert_eq!(result, Ok(vec![value(3), value(2)]), "{profile}");
        }
    }
}

#[test]
fn rawlen_environment_surface_tracks_profile_switches_on_one_vm() {
    let cases = [
        (SemanticProfile::Lua55, b"return rawlen ~= nil".as_slice()),
        (SemanticProfile::Lua51, b"return rawlen == nil".as_slice()),
        (SemanticProfile::Lua52, b"return rawlen ~= nil".as_slice()),
        (SemanticProfile::Lua51, b"return rawlen == nil".as_slice()),
        (SemanticProfile::Blu, b"return rawlen ~= nil".as_slice()),
    ];
    let mut vm = Vm::default();
    for (profile, source) in cases {
        let compiled = OwnedCompiler::default()
            .compile(&make_source(source.to_vec()), profile, compiler_identity())
            .unwrap();
        assert_eq!(
            vm.execute_blu_v1(compiled.into_validated_artifact(), BluLimits::default()),
            Ok(vec![Value::Boolean(true)]),
            "{profile}"
        );
    }
}

#[test]
fn base_library_counts_follow_profile_numeric_subtypes() {
    let source = make_source(
        b"local first,second=string.byte('AZ',1,2) local next_key=next({10}) local iterator,state,initial=ipairs({10}) local ipairs_key=iterator(state,initial) return select('#',10,20,30),string.len('abc'),first,second,next_key,initial,ipairs_key"
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
        let integral = |value| {
            if modern {
                Value::Integer(value)
            } else {
                Value::Number(value as f64)
            }
        };
        assert_eq!(
            Vm::default().execute_blu_v1(compiled.into_validated_artifact(), BluLimits::default()),
            Ok(vec![
                integral(3),
                integral(3),
                integral(65),
                integral(90),
                integral(1),
                integral(0),
                integral(1),
            ]),
            "{profile}"
        );
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
        b"local left local right local handler handler = setmetatable({}, {__call = function(self, a, b) return rawequal(self, handler) and rawequal(a, left) and (rawequal(b, right) or rawequal(b, left) or b == nil) end}) local mt = {__add = handler, __unm = handler, __concat = handler, __eq = handler, __lt = handler, __le = handler, __len = function() return 7 end} left = setmetatable({}, mt) right = setmetatable({}, mt) return left + right, -left, left .. right, left == right, left < right, left <= right, #left"
            .to_vec(),
    );
    for profile in SemanticProfile::ALL {
        let compiled = OwnedCompiler::default()
            .compile(&source, profile, compiler_identity())
            .unwrap();
        let length = match profile {
            SemanticProfile::Lua51 => Value::Number(0.0),
            SemanticProfile::Lua53 | SemanticProfile::Lua54 | SemanticProfile::Lua55 => {
                Value::Integer(7)
            }
            _ => Value::Number(7.0),
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
