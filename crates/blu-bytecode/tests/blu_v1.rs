use blu_bytecode::blu::{
    Artifact, BLU_V1_VERSION, BluLimits, BytecodeFormat, Constant, DecodeError, FeatureBits,
    Instruction, LocalDebug, MAGIC, Prototype, SourceRecord, TranslationError, Upvalue,
    UpvalueDebug, ValidatedArtifact, ValidationError, decode, decode_validated, encode,
    instruction_is_legal, translate_baseline_to_luau,
};
use blu_bytecode::{ChunkError, Constant as LuauConstant, LoadLimits, Opcode, load};
use blu_core::{
    ByteSpan, CompilerId, CompilerIdentity, IdentityLimits, SemanticProfile, SourceId,
    SourceIdentity,
};

type LimitCase = (fn(&mut BluLimits), &'static str);

fn read_u32(bytes: &[u8], offset: &mut usize) -> u32 {
    let value = u32::from_le_bytes(bytes[*offset..*offset + 4].try_into().unwrap());
    *offset += 4;
    value
}

fn skip_blob(bytes: &[u8], offset: &mut usize) {
    let length = read_u32(bytes, offset) as usize;
    *offset += length;
}

fn source_count_offset(bytes: &[u8]) -> usize {
    let mut offset = 8 + 16;
    skip_blob(bytes, &mut offset);
    skip_blob(bytes, &mut offset);
    let revision = bytes[offset];
    offset += 1;
    if revision == 1 {
        skip_blob(bytes, &mut offset);
    }
    offset
}

fn first_prototype_offset(bytes: &[u8]) -> usize {
    let mut offset = source_count_offset(bytes);
    let source_count = read_u32(bytes, &mut offset);
    for _ in 0..source_count {
        offset += 4;
        skip_blob(bytes, &mut offset);
        offset += 4 + 32;
    }
    let prototype_count = read_u32(bytes, &mut offset);
    assert!(prototype_count > 0);
    offset + 4
}

fn first_constant_and_instruction_offsets(bytes: &[u8]) -> (usize, usize) {
    let prototype = first_prototype_offset(bytes);
    let mut offset = prototype + 20;
    let constant_count = read_u32(bytes, &mut offset);
    let first_constant = offset;
    for _ in 0..constant_count {
        let tag = bytes[offset];
        offset += match tag {
            0..=2 => 1,
            3 => 9,
            4 => {
                let mut length_offset = offset + 1;
                1 + 4 + read_u32(bytes, &mut length_offset) as usize
            }
            5 => 9,
            _ => panic!("fixture has an invalid constant tag"),
        };
    }
    let upvalues = read_u32(bytes, &mut offset) as usize;
    offset += upvalues * 3;
    let children = read_u32(bytes, &mut offset) as usize;
    offset += children * 4;
    let code = read_u32(bytes, &mut offset);
    assert!(code > 0);
    (first_constant, offset)
}

fn fixture() -> Artifact {
    let identity_limits = IdentityLimits::default();
    let source_id = SourceId::new(7);
    let source = SourceRecord {
        identity: SourceIdentity::new(source_id, "test.blu", identity_limits).unwrap(),
        byte_len: 80,
        digest: [0x5a; 32],
    };
    let compiler = CompilerIdentity::new(
        CompilerId::new([0x42; 16]),
        "blu",
        "0.1",
        None,
        identity_limits,
    )
    .unwrap();
    let span = |start, end| ByteSpan::from_usize(source_id, start, end).unwrap();

    Artifact {
        format: BytecodeFormat::BluV1,
        compiler,
        sources: vec![source],
        main: 0,
        prototypes: vec![
            Prototype {
                profile: SemanticProfile::Lua54,
                source: source_id,
                register_count: 3,
                parameter_count: 0,
                is_vararg: false,
                required_features: FeatureBits::BASELINE,
                constants: vec![Constant::Number(40.0), Constant::Number(2.0)],
                upvalues: vec![],
                children: vec![1],
                code: vec![
                    Instruction::LoadConstant {
                        destination: 0,
                        constant: 0,
                    },
                    Instruction::LoadConstant {
                        destination: 1,
                        constant: 1,
                    },
                    Instruction::Add {
                        destination: 2,
                        left: 0,
                        right: 1,
                    },
                    Instruction::Return { first: 2, count: 1 },
                ],
                source_map: vec![span(34, 36), span(71, 72), span(34, 72), span(64, 72)],
                locals: vec![LocalDebug {
                    name: b"answer".to_vec(),
                    register: 0,
                    start_pc: 1,
                    end_pc: 4,
                }],
                upvalue_debug: vec![],
            },
            Prototype {
                profile: SemanticProfile::Lua51,
                source: source_id,
                register_count: 1,
                parameter_count: 1,
                is_vararg: true,
                required_features: FeatureBits::BASELINE,
                constants: vec![
                    Constant::Nil,
                    Constant::Boolean(true),
                    Constant::String(vec![0]),
                ],
                upvalues: vec![Upvalue::ParentRegister(0)],
                children: vec![],
                code: vec![Instruction::Return { first: 0, count: 1 }],
                source_map: vec![span(0, 0)],
                locals: vec![],
                upvalue_debug: vec![UpvalueDebug {
                    name: b"captured".to_vec(),
                    upvalue: 0,
                    start_pc: 0,
                    end_pc: 1,
                }],
            },
        ],
    }
}

fn baseline_fixture(profile: SemanticProfile) -> Artifact {
    let mut artifact = fixture();
    artifact.prototypes.truncate(1);
    artifact.prototypes[0].profile = profile;
    artifact.prototypes[0].children.clear();
    artifact
}

fn floor_division_fixture(profile: SemanticProfile) -> Artifact {
    let mut artifact = baseline_fixture(profile);
    artifact.prototypes[0].required_features = FeatureBits::BASELINE | FeatureBits::FLOOR_DIVISION;
    artifact.prototypes[0].code[2] = Instruction::FloorDivide {
        destination: 2,
        left: 0,
        right: 1,
    };
    artifact
}

fn bitwise_fixture(profile: SemanticProfile, instruction: Instruction) -> Artifact {
    let mut artifact = baseline_fixture(profile);
    artifact.prototypes[0].required_features =
        FeatureBits::BASELINE | FeatureBits::BITWISE_OPERATORS;
    artifact.prototypes[0].code[2] = instruction;
    artifact
}

fn concatenation_fixture(profile: SemanticProfile) -> Artifact {
    let mut artifact = baseline_fixture(profile);
    artifact.prototypes[0].required_features = FeatureBits::BASELINE | FeatureBits::CONCATENATION;
    artifact.prototypes[0].code[2] = Instruction::Concatenate {
        destination: 2,
        left: 0,
        right: 1,
    };
    artifact
}

fn comparison_fixture(profile: SemanticProfile, instruction: Instruction) -> Artifact {
    let mut artifact = baseline_fixture(profile);
    artifact.prototypes[0].required_features = FeatureBits::BASELINE | FeatureBits::COMPARISONS;
    artifact.prototypes[0].code[2] = instruction;
    artifact
}

fn closure_fixture() -> Artifact {
    let mut artifact = fixture();
    let span = ByteSpan::from_usize(SourceId::new(7), 0, 0).unwrap();
    artifact.prototypes[0].register_count = 2;
    artifact.prototypes[0].required_features = FeatureBits::BASELINE | FeatureBits::CLOSURES;
    artifact.prototypes[0].constants = vec![Constant::Number(42.0)];
    artifact.prototypes[0].code = vec![
        Instruction::LoadConstant {
            destination: 0,
            constant: 0,
        },
        Instruction::NewClosure {
            destination: 1,
            child: 0,
        },
        Instruction::Return { first: 1, count: 1 },
    ];
    artifact.prototypes[0].source_map = vec![span; 3];
    artifact.prototypes[0].locals.clear();
    artifact.prototypes[1].register_count = 2;
    artifact.prototypes[1].parameter_count = 1;
    artifact.prototypes[1].is_vararg = false;
    artifact.prototypes[1].required_features = FeatureBits::BASELINE | FeatureBits::CLOSURES;
    artifact.prototypes[1].constants.clear();
    artifact.prototypes[1].code = vec![
        Instruction::GetUpvalue {
            destination: 1,
            upvalue: 0,
        },
        Instruction::SetUpvalue {
            upvalue: 0,
            source: 0,
        },
        Instruction::Return { first: 1, count: 1 },
    ];
    artifact.prototypes[1].source_map = vec![span; 3];
    artifact
}

fn forward_branch_fixture(profile: SemanticProfile) -> Artifact {
    let mut artifact = baseline_fixture(profile);
    artifact.prototypes[0].required_features =
        FeatureBits::BASELINE | FeatureBits::FORWARD_BRANCHES;
    artifact.prototypes[0].code[2] = Instruction::JumpIfTruthy {
        condition: 0,
        target: 3,
    };
    artifact.prototypes[0].code[3] = Instruction::Return { first: 0, count: 1 };
    artifact
}

#[test]
fn canonical_round_trip_preserves_profiles_and_metadata() {
    let limits = BluLimits::default();
    let validated = ValidatedArtifact::new(fixture(), limits).unwrap();
    let bytes = encode(&validated, limits).unwrap();

    assert_eq!(&bytes[..4], &MAGIC);
    assert_eq!(
        u16::from_le_bytes(bytes[4..6].try_into().unwrap()),
        BLU_V1_VERSION
    );

    let decoded = decode_validated(&bytes, limits).unwrap();
    assert_eq!(decoded, validated);
    assert_eq!(encode(&decoded, limits).unwrap(), bytes);
    assert_eq!(decoded.main().profile, SemanticProfile::Lua54);
    assert_eq!(decoded.prototypes()[1].profile, SemanticProfile::Lua51);
    assert_eq!(
        decoded.prototypes()[1].upvalues,
        [Upvalue::ParentRegister(0)]
    );
    assert_eq!(decoded.validation_policy(), limits);
}

#[test]
fn fixed_multi_result_calls_round_trip_and_require_their_feature() {
    let artifact_with_features = |required_features| {
        let mut artifact = fixture();
        let prototype = &mut artifact.prototypes[0];
        prototype.register_count = 4;
        prototype.required_features = required_features;
        prototype.code = vec![
            Instruction::LoadConstant {
                destination: 0,
                constant: 0,
            },
            Instruction::CallResults {
                destination: 1,
                function: 0,
                arguments: 0,
                argument_count: 0,
                result_count: 3,
            },
            Instruction::Return { first: 1, count: 3 },
        ];
        prototype.source_map.truncate(3);
        prototype.locals.clear();
        artifact
    };

    let limits = BluLimits::default();
    let validated = ValidatedArtifact::new(
        artifact_with_features(FeatureBits::BASELINE | FeatureBits::FIXED_MULTI_RESULTS),
        limits,
    )
    .unwrap();
    let bytes = encode(&validated, limits).unwrap();
    assert_eq!(decode_validated(&bytes, limits).unwrap(), validated);

    assert_eq!(
        ValidatedArtifact::new(artifact_with_features(FeatureBits::BASELINE), limits),
        Err(ValidationError::MissingFeature {
            prototype: 0,
            feature: "fixed multi-result calls",
        })
    );
}

#[test]
fn return_calls_round_trip_terminate_prototypes_and_require_their_feature() {
    let artifact_with_features = |required_features| {
        let mut artifact = fixture();
        let prototype = &mut artifact.prototypes[0];
        prototype.required_features = required_features;
        prototype.code = vec![
            Instruction::LoadConstant {
                destination: 0,
                constant: 0,
            },
            Instruction::ReturnCall {
                function: 0,
                arguments: 0,
                argument_count: 0,
            },
        ];
        prototype.source_map.truncate(2);
        prototype.locals.clear();
        artifact
    };

    let limits = BluLimits::default();
    let validated = ValidatedArtifact::new(
        artifact_with_features(FeatureBits::BASELINE | FeatureBits::RETURN_CALLS),
        limits,
    )
    .unwrap();
    let bytes = encode(&validated, limits).unwrap();
    assert_eq!(decode_validated(&bytes, limits).unwrap(), validated);

    assert_eq!(
        ValidatedArtifact::new(artifact_with_features(FeatureBits::BASELINE), limits),
        Err(ValidationError::MissingFeature {
            prototype: 0,
            feature: "return calls",
        })
    );
}

#[test]
fn prefixed_return_calls_round_trip_with_validated_ranges() {
    let mut artifact = fixture();
    let prototype = &mut artifact.prototypes[0];
    prototype.required_features = FeatureBits::BASELINE | FeatureBits::RETURN_CALLS;
    prototype.code = vec![
        Instruction::LoadConstant {
            destination: 0,
            constant: 0,
        },
        Instruction::LoadConstant {
            destination: 1,
            constant: 1,
        },
        Instruction::ReturnCallPrefix {
            first: 1,
            count: 1,
            function: 0,
            arguments: 0,
            argument_count: 0,
        },
    ];
    prototype.source_map.truncate(3);
    prototype.locals.clear();

    let limits = BluLimits::default();
    let validated = ValidatedArtifact::new(artifact, limits).unwrap();
    let bytes = encode(&validated, limits).unwrap();
    assert_eq!(decode_validated(&bytes, limits).unwrap(), validated);
}

#[test]
fn dynamic_vararg_returns_round_trip_and_require_vararg_metadata() {
    let artifact_with_metadata = |required_features, is_vararg| {
        let mut artifact = fixture();
        let prototype = &mut artifact.prototypes[0];
        prototype.is_vararg = is_vararg;
        prototype.required_features = required_features;
        prototype.code = vec![
            Instruction::LoadConstant {
                destination: 0,
                constant: 0,
            },
            Instruction::ReturnVarargs { first: 0, count: 1 },
        ];
        prototype.source_map.truncate(2);
        prototype.locals.clear();
        artifact
    };

    let limits = BluLimits::default();
    let validated = ValidatedArtifact::new(
        artifact_with_metadata(FeatureBits::BASELINE | FeatureBits::VARARGS, true),
        limits,
    )
    .unwrap();
    let bytes = encode(&validated, limits).unwrap();
    assert_eq!(decode_validated(&bytes, limits).unwrap(), validated);

    assert_eq!(
        ValidatedArtifact::new(artifact_with_metadata(FeatureBits::BASELINE, true), limits),
        Err(ValidationError::MissingFeature {
            prototype: 0,
            feature: "varargs",
        })
    );
    assert_eq!(
        ValidatedArtifact::new(
            artifact_with_metadata(FeatureBits::BASELINE | FeatureBits::VARARGS, false),
            limits
        ),
        Err(ValidationError::InvalidMetadata {
            prototype: 0,
            what: "vararg instruction in fixed-argument prototype",
        })
    );
}

#[test]
fn dynamic_vararg_calls_round_trip_canonically() {
    let limits = BluLimits::default();
    for code in [
        vec![
            Instruction::LoadConstant {
                destination: 0,
                constant: 0,
            },
            Instruction::LoadConstant {
                destination: 1,
                constant: 1,
            },
            Instruction::CallVarargsResults {
                destination: 2,
                function: 0,
                arguments: 1,
                argument_count: 1,
                result_count: 1,
            },
            Instruction::ReturnCallVarargsPrefix {
                first: 2,
                count: 1,
                function: 0,
                arguments: 1,
                argument_count: 1,
            },
        ],
        vec![
            Instruction::LoadConstant {
                destination: 0,
                constant: 0,
            },
            Instruction::ReturnCallVarargs {
                function: 0,
                arguments: 0,
                argument_count: 0,
            },
        ],
    ] {
        let mut artifact = fixture();
        let prototype = &mut artifact.prototypes[0];
        prototype.register_count = 3;
        prototype.is_vararg = true;
        prototype.required_features = FeatureBits::BASELINE
            | FeatureBits::FIXED_MULTI_RESULTS
            | FeatureBits::RETURN_CALLS
            | FeatureBits::VARARGS;
        prototype.source_map.truncate(code.len());
        prototype.locals.clear();
        prototype.code = code;
        let validated = ValidatedArtifact::new(artifact, limits).unwrap();
        let bytes = encode(&validated, limits).unwrap();
        assert_eq!(decode_validated(&bytes, limits).unwrap(), validated);
    }
}

#[test]
fn dynamic_vararg_table_lists_round_trip_with_validated_starts() {
    let artifact_with_start = |start| {
        let mut artifact = fixture();
        let prototype = &mut artifact.prototypes[0];
        prototype.register_count = 1;
        prototype.is_vararg = true;
        prototype.required_features =
            FeatureBits::BASELINE | FeatureBits::TABLES | FeatureBits::VARARGS;
        prototype.code = vec![
            Instruction::NewTable { destination: 0 },
            Instruction::SetListVarargs { table: 0, start },
            Instruction::Return { first: 0, count: 1 },
        ];
        prototype.source_map.truncate(3);
        prototype.locals.clear();
        artifact
    };

    let limits = BluLimits::default();
    let validated = ValidatedArtifact::new(artifact_with_start(1), limits).unwrap();
    let bytes = encode(&validated, limits).unwrap();
    assert_eq!(decode_validated(&bytes, limits).unwrap(), validated);
    assert_eq!(
        ValidatedArtifact::new(artifact_with_start(0), limits),
        Err(ValidationError::InvalidInstruction {
            prototype: 0,
            pc: 1,
            what: "vararg table-list start must be positive",
        })
    );
}

#[test]
fn dynamic_call_table_lists_round_trip_and_require_their_feature() {
    let artifact_with = |instruction, required_features, is_vararg| {
        let mut artifact = fixture();
        let prototype = &mut artifact.prototypes[0];
        prototype.register_count = 2;
        prototype.is_vararg = is_vararg;
        prototype.required_features = required_features;
        prototype.code = vec![
            Instruction::NewTable { destination: 0 },
            Instruction::LoadConstant {
                destination: 1,
                constant: 0,
            },
            instruction,
            Instruction::Return { first: 0, count: 1 },
        ];
        prototype.source_map.truncate(4);
        prototype.locals.clear();
        artifact
    };
    let plain = Instruction::SetListCall {
        table: 0,
        start: 1,
        function: 1,
        arguments: 0,
        argument_count: 0,
    };
    let expanded = Instruction::SetListCallVarargs {
        table: 0,
        start: 1,
        function: 1,
        arguments: 0,
        argument_count: 0,
    };
    let limits = BluLimits::default();
    for (instruction, features, is_vararg) in [
        (
            plain,
            FeatureBits::BASELINE | FeatureBits::TABLES | FeatureBits::DYNAMIC_CALL_RESULTS,
            false,
        ),
        (
            expanded,
            FeatureBits::BASELINE
                | FeatureBits::TABLES
                | FeatureBits::VARARGS
                | FeatureBits::DYNAMIC_CALL_RESULTS,
            true,
        ),
    ] {
        let validated =
            ValidatedArtifact::new(artifact_with(instruction, features, is_vararg), limits)
                .unwrap();
        let bytes = encode(&validated, limits).unwrap();
        assert_eq!(decode_validated(&bytes, limits).unwrap(), validated);
    }
    assert_eq!(
        ValidatedArtifact::new(
            artifact_with(plain, FeatureBits::BASELINE | FeatureBits::TABLES, false),
            limits
        ),
        Err(ValidationError::MissingFeature {
            prototype: 0,
            feature: "dynamic call results",
        })
    );
}

#[test]
fn closure_instructions_round_trip_with_validated_capture_metadata() {
    let limits = BluLimits::default();
    let validated = ValidatedArtifact::new(closure_fixture(), limits).unwrap();
    let bytes = encode(&validated, limits).unwrap();
    let decoded = decode_validated(&bytes, limits).unwrap();
    assert_eq!(decoded, validated);
    assert_eq!(
        decoded.prototypes()[0].code[1],
        Instruction::NewClosure {
            destination: 1,
            child: 0
        }
    );
    assert_eq!(
        decoded.prototypes()[1].code[..2],
        [
            Instruction::GetUpvalue {
                destination: 1,
                upvalue: 0
            },
            Instruction::SetUpvalue {
                upvalue: 0,
                source: 0
            }
        ]
    );
}

#[test]
fn closure_validation_requires_features_and_initialized_captures() {
    let limits = BluLimits::default();
    let mut missing_feature = closure_fixture();
    missing_feature.prototypes[0].required_features = FeatureBits::BASELINE;
    assert_eq!(
        ValidatedArtifact::new(missing_feature, limits),
        Err(ValidationError::MissingFeature {
            prototype: 0,
            feature: "closures"
        })
    );

    let mut uninitialized = closure_fixture();
    uninitialized.prototypes[0].code.remove(0);
    uninitialized.prototypes[0].source_map.remove(0);
    assert!(matches!(
        ValidatedArtifact::new(uninitialized, limits),
        Err(ValidationError::InvalidInstruction {
            prototype: 0,
            pc: 0,
            what: "register is read before initialization"
        })
    ));
}

#[test]
fn pre_integer_baseline_encoding_remains_byte_for_byte_stable() {
    const EXPECTED: &[u8] = &[
        66, 76, 85, 0, 1, 0, 0, 0, 66, 66, 66, 66, 66, 66, 66, 66, 66, 66, 66, 66, 66, 66, 66, 66,
        3, 0, 0, 0, 98, 108, 117, 3, 0, 0, 0, 48, 46, 49, 0, 1, 0, 0, 0, 7, 0, 0, 0, 8, 0, 0, 0,
        116, 101, 115, 116, 46, 98, 108, 117, 80, 0, 0, 0, 90, 90, 90, 90, 90, 90, 90, 90, 90, 90,
        90, 90, 90, 90, 90, 90, 90, 90, 90, 90, 90, 90, 90, 90, 90, 90, 90, 90, 90, 90, 90, 90, 1,
        0, 0, 0, 0, 0, 0, 0, 1, 0, 0, 0, 7, 0, 0, 0, 3, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0, 2, 0, 0,
        0, 3, 0, 0, 0, 0, 0, 0, 68, 64, 3, 0, 0, 0, 0, 0, 0, 0, 64, 0, 0, 0, 0, 0, 0, 0, 0, 4, 0,
        0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1, 0, 1, 0, 0, 0, 1, 2, 0, 0, 0, 1, 0, 2, 2, 0, 1, 0, 4, 0,
        0, 0, 7, 0, 0, 0, 34, 0, 0, 0, 36, 0, 0, 0, 7, 0, 0, 0, 71, 0, 0, 0, 72, 0, 0, 0, 7, 0, 0,
        0, 34, 0, 0, 0, 72, 0, 0, 0, 7, 0, 0, 0, 64, 0, 0, 0, 72, 0, 0, 0, 1, 0, 0, 0, 6, 0, 0, 0,
        97, 110, 115, 119, 101, 114, 0, 0, 1, 0, 0, 0, 4, 0, 0, 0, 0, 0, 0, 0,
    ];
    let limits = BluLimits::default();
    let validated = ValidatedArtifact::new(baseline_fixture(SemanticProfile::Blu), limits).unwrap();
    assert_eq!(encode(&validated, limits).unwrap(), EXPECTED);
}

#[test]
fn all_seven_profile_tags_round_trip_canonically() {
    let limits = BluLimits::default();
    let golden = [
        (SemanticProfile::Blu, 1),
        (SemanticProfile::Luau, 2),
        (SemanticProfile::Lua51, 3),
        (SemanticProfile::Lua52, 4),
        (SemanticProfile::Lua53, 5),
        (SemanticProfile::Lua54, 6),
        (SemanticProfile::Lua55, 7),
    ];
    for (profile, tag) in golden {
        let mut artifact = fixture();
        artifact.prototypes[0].profile = profile;
        let validated = ValidatedArtifact::new(artifact, limits).unwrap();
        let bytes = encode(&validated, limits).unwrap();
        assert_eq!(bytes[first_prototype_offset(&bytes)], tag);
        let decoded = decode_validated(&bytes, limits).unwrap();
        assert_eq!(decoded.main().profile, profile);
        assert_eq!(encode(&decoded, limits).unwrap(), bytes);
    }
}

#[test]
fn baseline_instruction_legality_is_explicit_for_every_profile() {
    let instructions = [
        Instruction::LoadConstant {
            destination: 0,
            constant: 0,
        },
        Instruction::Move {
            destination: 1,
            source: 0,
        },
        Instruction::Not {
            destination: 1,
            source: 0,
        },
        Instruction::Negate {
            destination: 1,
            source: 0,
        },
        Instruction::Length {
            destination: 1,
            source: 0,
        },
        Instruction::Add {
            destination: 0,
            left: 0,
            right: 0,
        },
        Instruction::Subtract {
            destination: 0,
            left: 0,
            right: 0,
        },
        Instruction::Multiply {
            destination: 0,
            left: 0,
            right: 0,
        },
        Instruction::Divide {
            destination: 0,
            left: 0,
            right: 0,
        },
        Instruction::Modulo {
            destination: 0,
            left: 0,
            right: 0,
        },
        Instruction::Power {
            destination: 0,
            left: 0,
            right: 0,
        },
        Instruction::Concatenate {
            destination: 0,
            left: 0,
            right: 0,
        },
        Instruction::Equal {
            destination: 0,
            left: 0,
            right: 0,
        },
        Instruction::LessThan {
            destination: 0,
            left: 0,
            right: 0,
        },
        Instruction::LessEqual {
            destination: 0,
            left: 0,
            right: 0,
        },
        Instruction::Return { first: 0, count: 1 },
    ];

    for profile in SemanticProfile::ALL {
        for instruction in instructions {
            assert!(instruction_is_legal(profile, instruction));
        }
        // The round-trip fixture contains every baseline instruction, so this
        // also proves the validator routes each established profile through the
        // centralized table.
        let mut artifact = fixture();
        artifact.prototypes[0].profile = profile;
        assert!(ValidatedArtifact::new(artifact, BluLimits::default()).is_ok());
    }
}

#[test]
fn subtraction_is_canonical_profile_neutral_and_bootstrap_translatable() {
    let limits = BluLimits::default();
    for profile in SemanticProfile::ALL {
        let mut artifact = baseline_fixture(profile);
        artifact.prototypes[0].code[2] = Instruction::Subtract {
            destination: 2,
            left: 0,
            right: 1,
        };
        let validated = ValidatedArtifact::new(artifact, limits).unwrap();
        let bytes = encode(&validated, limits).unwrap();
        let (_, first_instruction) = first_constant_and_instruction_offsets(&bytes);
        assert_eq!(bytes[first_instruction + 14], 6, "{profile}");
        assert_eq!(
            decode_validated(&bytes, limits).unwrap().main().code[2],
            Instruction::Subtract {
                destination: 2,
                left: 0,
                right: 1,
            }
        );
    }

    let mut artifact = baseline_fixture(SemanticProfile::Blu);
    artifact.prototypes[0].code[2] = Instruction::Subtract {
        destination: 2,
        left: 0,
        right: 1,
    };
    let translated = translate_baseline_to_luau(
        ValidatedArtifact::new(artifact, limits).unwrap(),
        SemanticProfile::Blu,
        limits,
    )
    .unwrap()
    .into_validated_chunk()
    .into_chunk();
    assert_eq!(
        translated.prototypes[0].instructions[2].opcode(),
        Opcode::Sub
    );
}

#[test]
fn multiplication_is_canonical_profile_neutral_and_bootstrap_translatable() {
    let limits = BluLimits::default();
    for profile in SemanticProfile::ALL {
        let mut artifact = baseline_fixture(profile);
        artifact.prototypes[0].code[2] = Instruction::Multiply {
            destination: 2,
            left: 0,
            right: 1,
        };
        let validated = ValidatedArtifact::new(artifact, limits).unwrap();
        let bytes = encode(&validated, limits).unwrap();
        let (_, first_instruction) = first_constant_and_instruction_offsets(&bytes);
        assert_eq!(bytes[first_instruction + 14], 7, "{profile}");
        assert_eq!(
            decode_validated(&bytes, limits).unwrap().main().code[2],
            Instruction::Multiply {
                destination: 2,
                left: 0,
                right: 1,
            }
        );
    }

    let mut artifact = baseline_fixture(SemanticProfile::Blu);
    artifact.prototypes[0].code[2] = Instruction::Multiply {
        destination: 2,
        left: 0,
        right: 1,
    };
    let translated = translate_baseline_to_luau(
        ValidatedArtifact::new(artifact, limits).unwrap(),
        SemanticProfile::Blu,
        limits,
    )
    .unwrap()
    .into_validated_chunk()
    .into_chunk();
    assert_eq!(
        translated.prototypes[0].instructions[2].opcode(),
        Opcode::Mul
    );
}

#[test]
fn move_is_canonical_profile_neutral_and_bootstrap_translatable() {
    let limits = BluLimits::default();
    for profile in SemanticProfile::ALL {
        let mut artifact = baseline_fixture(profile);
        artifact.prototypes[0].code[2] = Instruction::Move {
            destination: 2,
            source: 0,
        };
        let validated = ValidatedArtifact::new(artifact, limits).unwrap();
        let bytes = encode(&validated, limits).unwrap();
        let (_, first_instruction) = first_constant_and_instruction_offsets(&bytes);
        assert_eq!(bytes[first_instruction + 14], 4, "{profile}");
        assert_eq!(
            decode_validated(&bytes, limits).unwrap().main().code[2],
            Instruction::Move {
                destination: 2,
                source: 0,
            }
        );
    }

    let artifact = baseline_fixture(SemanticProfile::Blu);
    let mut artifact = ValidatedArtifact::new(artifact, limits)
        .unwrap()
        .into_artifact();
    artifact.prototypes[0].code[2] = Instruction::Move {
        destination: 2,
        source: 0,
    };
    let translated = translate_baseline_to_luau(
        ValidatedArtifact::new(artifact, limits).unwrap(),
        SemanticProfile::Blu,
        limits,
    )
    .unwrap();
    let chunk = translated.into_validated_chunk().into_chunk();
    assert_eq!(chunk.prototypes[0].instructions[2].opcode(), Opcode::Move);
}

#[test]
fn not_is_canonical_profile_neutral_and_bootstrap_translatable() {
    let limits = BluLimits::default();
    for profile in SemanticProfile::ALL {
        let mut artifact = baseline_fixture(profile);
        artifact.prototypes[0].code[2] = Instruction::Not {
            destination: 2,
            source: 0,
        };
        let validated = ValidatedArtifact::new(artifact, limits).unwrap();
        let bytes = encode(&validated, limits).unwrap();
        let (_, first_instruction) = first_constant_and_instruction_offsets(&bytes);
        assert_eq!(bytes[first_instruction + 14], 5, "{profile}");
        assert_eq!(
            decode_validated(&bytes, limits).unwrap().main().code[2],
            Instruction::Not {
                destination: 2,
                source: 0,
            }
        );
    }

    let mut artifact = baseline_fixture(SemanticProfile::Blu);
    artifact.prototypes[0].code[2] = Instruction::Not {
        destination: 2,
        source: 0,
    };
    let translated = translate_baseline_to_luau(
        ValidatedArtifact::new(artifact, limits).unwrap(),
        SemanticProfile::Blu,
        limits,
    )
    .unwrap()
    .into_validated_chunk()
    .into_chunk();
    assert_eq!(
        translated.prototypes[0].instructions[2].opcode(),
        Opcode::Not
    );
}

#[test]
fn negation_is_canonical_profile_neutral_and_bootstrap_translatable() {
    let limits = BluLimits::default();
    for profile in SemanticProfile::ALL {
        let mut artifact = baseline_fixture(profile);
        artifact.prototypes[0].code[2] = Instruction::Negate {
            destination: 2,
            source: 0,
        };
        let validated = ValidatedArtifact::new(artifact, limits).unwrap();
        let bytes = encode(&validated, limits).unwrap();
        let (_, first_instruction) = first_constant_and_instruction_offsets(&bytes);
        assert_eq!(bytes[first_instruction + 14], 9, "{profile}");
        assert_eq!(
            decode_validated(&bytes, limits).unwrap().main().code[2],
            Instruction::Negate {
                destination: 2,
                source: 0,
            }
        );
    }

    let mut artifact = baseline_fixture(SemanticProfile::Blu);
    artifact.prototypes[0].code[2] = Instruction::Negate {
        destination: 2,
        source: 0,
    };
    let translated = translate_baseline_to_luau(
        ValidatedArtifact::new(artifact, limits).unwrap(),
        SemanticProfile::Blu,
        limits,
    )
    .unwrap()
    .into_validated_chunk()
    .into_chunk();
    assert_eq!(
        translated.prototypes[0].instructions[2].opcode(),
        Opcode::Minus
    );
}

#[test]
fn length_is_canonical_profile_neutral_and_bootstrap_translatable() {
    let limits = BluLimits::default();
    for profile in SemanticProfile::ALL {
        let mut artifact = baseline_fixture(profile);
        artifact.prototypes[0].code[2] = Instruction::Length {
            destination: 2,
            source: 0,
        };
        let validated = ValidatedArtifact::new(artifact, limits).unwrap();
        let bytes = encode(&validated, limits).unwrap();
        let (_, first_instruction) = first_constant_and_instruction_offsets(&bytes);
        assert_eq!(bytes[first_instruction + 14], 12, "{profile}");
        assert_eq!(
            decode_validated(&bytes, limits).unwrap().main().code[2],
            Instruction::Length {
                destination: 2,
                source: 0,
            }
        );
    }

    let mut artifact = baseline_fixture(SemanticProfile::Blu);
    artifact.prototypes[0].code[2] = Instruction::Length {
        destination: 2,
        source: 0,
    };
    let translated = translate_baseline_to_luau(
        ValidatedArtifact::new(artifact, limits).unwrap(),
        SemanticProfile::Blu,
        limits,
    )
    .unwrap()
    .into_validated_chunk()
    .into_chunk();
    assert_eq!(
        translated.prototypes[0].instructions[2].opcode(),
        Opcode::Length
    );
}

#[test]
fn zero_result_return_is_legal_for_every_profile_and_translates_without_a_register() {
    let limits = BluLimits::default();
    for profile in SemanticProfile::ALL {
        let mut artifact = baseline_fixture(profile);
        let prototype = &mut artifact.prototypes[0];
        prototype.register_count = 0;
        prototype.constants.clear();
        prototype.code = vec![Instruction::Return { first: 0, count: 0 }];
        prototype.source_map.truncate(1);
        prototype.locals.clear();
        assert!(
            ValidatedArtifact::new(artifact, limits).is_ok(),
            "{profile}"
        );
    }

    for profile in [SemanticProfile::Blu, SemanticProfile::Luau] {
        let mut artifact = baseline_fixture(profile);
        let prototype = &mut artifact.prototypes[0];
        prototype.register_count = 0;
        prototype.constants.clear();
        prototype.code = vec![Instruction::Return { first: 0, count: 0 }];
        prototype.source_map.truncate(1);
        prototype.locals.clear();
        let translated = translate_baseline_to_luau(
            ValidatedArtifact::new(artifact, limits).unwrap(),
            profile,
            limits,
        )
        .unwrap()
        .into_validated_chunk()
        .into_chunk();
        let instruction = translated.prototypes[0].instructions[0];
        assert_eq!(instruction.opcode(), Opcode::Return);
        assert_eq!(instruction.a(), 0);
        assert_eq!(instruction.b(), 1);
    }
}

#[test]
fn division_is_canonical_profile_neutral_and_bootstrap_translatable() {
    let limits = BluLimits::default();
    for profile in SemanticProfile::ALL {
        let mut artifact = baseline_fixture(profile);
        artifact.prototypes[0].code[2] = Instruction::Divide {
            destination: 2,
            left: 0,
            right: 1,
        };
        let validated = ValidatedArtifact::new(artifact, limits).unwrap();
        let bytes = encode(&validated, limits).unwrap();
        let (_, first_instruction) = first_constant_and_instruction_offsets(&bytes);
        assert_eq!(bytes[first_instruction + 14], 8, "{profile}");
        assert_eq!(
            decode_validated(&bytes, limits).unwrap().main().code[2],
            Instruction::Divide {
                destination: 2,
                left: 0,
                right: 1,
            }
        );
    }

    let mut artifact = baseline_fixture(SemanticProfile::Blu);
    artifact.prototypes[0].code[2] = Instruction::Divide {
        destination: 2,
        left: 0,
        right: 1,
    };
    let translated = translate_baseline_to_luau(
        ValidatedArtifact::new(artifact, limits).unwrap(),
        SemanticProfile::Blu,
        limits,
    )
    .unwrap()
    .into_validated_chunk()
    .into_chunk();
    assert_eq!(
        translated.prototypes[0].instructions[2].opcode(),
        Opcode::Div
    );
}

#[test]
fn modulo_is_canonical_profile_neutral_and_bootstrap_translatable() {
    let limits = BluLimits::default();
    for profile in SemanticProfile::ALL {
        let mut artifact = baseline_fixture(profile);
        artifact.prototypes[0].code[2] = Instruction::Modulo {
            destination: 2,
            left: 0,
            right: 1,
        };
        let validated = ValidatedArtifact::new(artifact, limits).unwrap();
        let bytes = encode(&validated, limits).unwrap();
        let (_, first_instruction) = first_constant_and_instruction_offsets(&bytes);
        assert_eq!(bytes[first_instruction + 14], 10, "{profile}");
        assert_eq!(
            decode_validated(&bytes, limits).unwrap().main().code[2],
            Instruction::Modulo {
                destination: 2,
                left: 0,
                right: 1,
            }
        );
    }

    let mut artifact = baseline_fixture(SemanticProfile::Blu);
    artifact.prototypes[0].code[2] = Instruction::Modulo {
        destination: 2,
        left: 0,
        right: 1,
    };
    let translated = translate_baseline_to_luau(
        ValidatedArtifact::new(artifact, limits).unwrap(),
        SemanticProfile::Blu,
        limits,
    )
    .unwrap()
    .into_validated_chunk()
    .into_chunk();
    assert_eq!(
        translated.prototypes[0].instructions[2].opcode(),
        Opcode::Mod
    );
}

#[test]
fn power_is_canonical_profile_neutral_and_bootstrap_translatable() {
    let limits = BluLimits::default();
    for profile in SemanticProfile::ALL {
        let mut artifact = baseline_fixture(profile);
        artifact.prototypes[0].code[2] = Instruction::Power {
            destination: 2,
            left: 0,
            right: 1,
        };
        let validated = ValidatedArtifact::new(artifact, limits).unwrap();
        let bytes = encode(&validated, limits).unwrap();
        let (_, first_instruction) = first_constant_and_instruction_offsets(&bytes);
        assert_eq!(bytes[first_instruction + 14], 11, "{profile}");
        assert_eq!(
            decode_validated(&bytes, limits).unwrap().main().code[2],
            Instruction::Power {
                destination: 2,
                left: 0,
                right: 1,
            }
        );
    }

    let mut artifact = baseline_fixture(SemanticProfile::Blu);
    artifact.prototypes[0].code[2] = Instruction::Power {
        destination: 2,
        left: 0,
        right: 1,
    };
    let translated = translate_baseline_to_luau(
        ValidatedArtifact::new(artifact, limits).unwrap(),
        SemanticProfile::Blu,
        limits,
    )
    .unwrap()
    .into_validated_chunk()
    .into_chunk();
    assert_eq!(
        translated.prototypes[0].instructions[2].opcode(),
        Opcode::Pow
    );
}

#[test]
fn comparison_wire_feature_is_explicit_and_profile_neutral() {
    let limits = BluLimits::default();
    let instructions = [
        Instruction::Equal {
            destination: 2,
            left: 0,
            right: 1,
        },
        Instruction::LessThan {
            destination: 2,
            left: 0,
            right: 1,
        },
        Instruction::LessEqual {
            destination: 2,
            left: 0,
            right: 1,
        },
    ];
    for profile in SemanticProfile::ALL {
        for instruction in instructions {
            assert!(instruction_is_legal(profile, instruction), "{profile}");
            let validated =
                ValidatedArtifact::new(comparison_fixture(profile, instruction), limits).unwrap();
            let bytes = encode(&validated, limits).unwrap();
            let decoded = decode_validated(&bytes, limits).unwrap();
            assert!(
                decoded
                    .main()
                    .required_features
                    .contains(FeatureBits::COMPARISONS)
            );
            assert_eq!(decoded.main().code[2], instruction);
            assert_eq!(encode(&decoded, limits).unwrap(), bytes);
        }

        let mut missing = comparison_fixture(profile, instructions[0]);
        missing.prototypes[0].required_features = FeatureBits::BASELINE;
        assert_eq!(
            ValidatedArtifact::new(missing, limits),
            Err(ValidationError::MissingFeature {
                prototype: 0,
                feature: "comparisons",
            })
        );
    }
}

#[test]
fn forward_branches_are_feature_gated_and_merge_register_initialization() {
    let limits = BluLimits::default();
    for profile in SemanticProfile::ALL {
        let artifact = forward_branch_fixture(profile);
        let validated = ValidatedArtifact::new(artifact, limits).unwrap();
        let bytes = encode(&validated, limits).unwrap();
        let decoded = decode_validated(&bytes, limits).unwrap();
        assert!(
            decoded
                .main()
                .required_features
                .contains(FeatureBits::FORWARD_BRANCHES),
            "{profile}"
        );
        assert_eq!(
            decoded.main().code[2],
            Instruction::JumpIfTruthy {
                condition: 0,
                target: 3,
            },
            "{profile}"
        );

        let mut unconditional = forward_branch_fixture(profile);
        unconditional.prototypes[0].code[2] = Instruction::Jump { target: 3 };
        let unconditional = ValidatedArtifact::new(unconditional, limits).unwrap();
        let bytes = encode(&unconditional, limits).unwrap();
        assert_eq!(
            decode_validated(&bytes, limits).unwrap().main().code[2],
            Instruction::Jump { target: 3 },
            "{profile}"
        );

        let mut missing = forward_branch_fixture(profile);
        missing.prototypes[0].required_features = FeatureBits::BASELINE;
        assert_eq!(
            ValidatedArtifact::new(missing, limits),
            Err(ValidationError::MissingFeature {
                prototype: 0,
                feature: "forward branches",
            }),
            "{profile}"
        );
    }

    let mut skipped_initialization = baseline_fixture(SemanticProfile::Blu);
    skipped_initialization.prototypes[0].required_features =
        FeatureBits::BASELINE | FeatureBits::FORWARD_BRANCHES;
    skipped_initialization.prototypes[0].code[1] = Instruction::JumpIfFalsy {
        condition: 0,
        target: 3,
    };
    skipped_initialization.prototypes[0].code[2] = Instruction::LoadConstant {
        destination: 1,
        constant: 1,
    };
    skipped_initialization.prototypes[0].code[3] = Instruction::Return { first: 1, count: 1 };
    assert!(matches!(
        ValidatedArtifact::new(skipped_initialization, limits),
        Err(ValidationError::InvalidInstruction {
            what: "register is read before initialization",
            ..
        })
    ));

    let mut backward = forward_branch_fixture(SemanticProfile::Blu);
    backward.prototypes[0].code[2] = Instruction::JumpIfTruthy {
        condition: 0,
        target: 1,
    };
    assert!(matches!(
        ValidatedArtifact::new(backward, limits),
        Err(ValidationError::InvalidInstruction {
            what: "branch target must be a later instruction",
            ..
        })
    ));

    let mut loop_artifact = baseline_fixture(SemanticProfile::Blu);
    loop_artifact.prototypes[0].required_features =
        FeatureBits::BASELINE | FeatureBits::FORWARD_BRANCHES | FeatureBits::BACKWARD_BRANCHES;
    loop_artifact.prototypes[0].code[1] = Instruction::JumpIfFalsy {
        condition: 0,
        target: 3,
    };
    loop_artifact.prototypes[0].code[2] = Instruction::Jump { target: 1 };
    loop_artifact.prototypes[0].code[3] = Instruction::Return { first: 0, count: 1 };
    let validated = ValidatedArtifact::new(loop_artifact, limits).unwrap();
    let bytes = encode(&validated, limits).unwrap();
    assert_eq!(
        decode_validated(&bytes, limits).unwrap().main().code[2],
        Instruction::Jump { target: 1 }
    );
    let mut loop_artifact = baseline_fixture(SemanticProfile::Blu);
    loop_artifact.prototypes[0].code[1] = Instruction::JumpIfFalsy {
        condition: 0,
        target: 3,
    };
    loop_artifact.prototypes[0].code[2] = Instruction::Jump { target: 1 };
    loop_artifact.prototypes[0].code[3] = Instruction::Return { first: 0, count: 1 };
    loop_artifact.prototypes[0].required_features =
        FeatureBits::BASELINE | FeatureBits::FORWARD_BRANCHES;
    assert_eq!(
        ValidatedArtifact::new(loop_artifact, limits),
        Err(ValidationError::MissingFeature {
            prototype: 0,
            feature: "backward branches",
        })
    );
}

#[test]
fn concatenation_wire_feature_is_explicit_and_profile_neutral() {
    let limits = BluLimits::default();
    let instruction = Instruction::Concatenate {
        destination: 2,
        left: 0,
        right: 1,
    };
    for profile in SemanticProfile::ALL {
        assert!(instruction_is_legal(profile, instruction), "{profile}");

        let mut missing = concatenation_fixture(profile);
        missing.prototypes[0].required_features = FeatureBits::BASELINE;
        assert_eq!(
            ValidatedArtifact::new(missing, limits),
            Err(ValidationError::MissingFeature {
                prototype: 0,
                feature: "concatenation",
            })
        );

        let validated = ValidatedArtifact::new(concatenation_fixture(profile), limits).unwrap();
        let bytes = encode(&validated, limits).unwrap();
        let decoded = decode_validated(&bytes, limits).unwrap();
        assert!(
            decoded
                .main()
                .required_features
                .contains(FeatureBits::CONCATENATION)
        );
        assert_eq!(decoded.main().code[2], instruction);
        assert_eq!(encode(&decoded, limits).unwrap(), bytes);
    }
}

#[test]
fn floor_division_wire_feature_and_profile_legality_are_explicit() {
    let limits = BluLimits::default();
    let instruction = Instruction::FloorDivide {
        destination: 2,
        left: 0,
        right: 1,
    };

    for profile in SemanticProfile::ALL {
        let legal = matches!(
            profile,
            SemanticProfile::Blu
                | SemanticProfile::Luau
                | SemanticProfile::Lua53
                | SemanticProfile::Lua54
                | SemanticProfile::Lua55
        );
        assert_eq!(
            instruction_is_legal(profile, instruction),
            legal,
            "{profile}"
        );

        if legal {
            let mut missing = floor_division_fixture(profile);
            missing.prototypes[0].required_features = FeatureBits::BASELINE;
            assert_eq!(
                ValidatedArtifact::new(missing, limits),
                Err(ValidationError::MissingFeature {
                    prototype: 0,
                    feature: "floor division",
                })
            );

            let validated =
                ValidatedArtifact::new(floor_division_fixture(profile), limits).unwrap();
            let bytes = encode(&validated, limits).unwrap();
            let (_, first_instruction) = first_constant_and_instruction_offsets(&bytes);
            assert_eq!(bytes[first_instruction + 14], 3, "{profile}");
            let decoded = decode_validated(&bytes, limits).unwrap();
            assert!(
                decoded
                    .main()
                    .required_features
                    .contains(FeatureBits::FLOOR_DIVISION)
            );
            assert_eq!(decoded.main().code[2], instruction);
        } else {
            assert_eq!(
                ValidatedArtifact::new(floor_division_fixture(profile), limits),
                Err(ValidationError::FeatureNotLegal {
                    prototype: 0,
                    feature: "floor division",
                    profile,
                })
            );
        }
    }
}

#[test]
fn bitwise_wire_feature_profile_legality_and_tags_are_explicit() {
    let limits = BluLimits::default();
    let instructions = [
        Instruction::BitwiseAnd {
            destination: 2,
            left: 0,
            right: 1,
        },
        Instruction::BitwiseOr {
            destination: 2,
            left: 0,
            right: 1,
        },
        Instruction::BitwiseExclusiveOr {
            destination: 2,
            left: 0,
            right: 1,
        },
        Instruction::ShiftLeft {
            destination: 2,
            left: 0,
            right: 1,
        },
        Instruction::ShiftRight {
            destination: 2,
            left: 0,
            right: 1,
        },
        Instruction::BitwiseNot {
            destination: 2,
            source: 0,
        },
    ];
    for profile in SemanticProfile::ALL {
        let legal = matches!(
            profile,
            SemanticProfile::Blu
                | SemanticProfile::Lua53
                | SemanticProfile::Lua54
                | SemanticProfile::Lua55
        );
        for (offset, instruction) in instructions.into_iter().enumerate() {
            assert_eq!(
                instruction_is_legal(profile, instruction),
                legal,
                "{profile}: {instruction:?}"
            );
            let fixture = bitwise_fixture(profile, instruction);
            if legal {
                let validated = ValidatedArtifact::new(fixture, limits).unwrap();
                let bytes = encode(&validated, limits).unwrap();
                let decoded = decode_validated(&bytes, limits).unwrap();
                assert_eq!(decoded.main().code[2], instruction);
                let (_, first_instruction) = first_constant_and_instruction_offsets(&bytes);
                assert_eq!(bytes[first_instruction + 14], 40 + offset as u8);
                assert_eq!(encode(&decoded, limits).unwrap(), bytes);
            } else {
                assert_eq!(
                    ValidatedArtifact::new(fixture, limits),
                    Err(ValidationError::FeatureNotLegal {
                        prototype: 0,
                        feature: "bitwise operators",
                        profile,
                    })
                );
            }
        }
    }

    let mut missing = bitwise_fixture(SemanticProfile::Blu, instructions[0]);
    missing.prototypes[0].required_features = FeatureBits::BASELINE;
    assert_eq!(
        ValidatedArtifact::new(missing, limits),
        Err(ValidationError::MissingFeature {
            prototype: 0,
            feature: "bitwise operators",
        })
    );

    let artifact = ValidatedArtifact::new(
        bitwise_fixture(SemanticProfile::Blu, instructions[0]),
        limits,
    )
    .unwrap();
    assert_eq!(
        translate_baseline_to_luau(artifact, SemanticProfile::Blu, limits),
        Err(TranslationError::UnsupportedInstruction {
            prototype: 0,
            instruction: "64-bit bitwise operators",
        })
    );
}

#[test]
fn luau_bootstrap_translation_rejects_floor_division_explicitly() {
    let limits = BluLimits::default();
    let artifact =
        ValidatedArtifact::new(floor_division_fixture(SemanticProfile::Luau), limits).unwrap();
    assert_eq!(
        translate_baseline_to_luau(artifact, SemanticProfile::Luau, limits),
        Err(TranslationError::UnsupportedInstruction {
            prototype: 0,
            instruction: "floor division",
        })
    );

    let mut artifact = floor_division_fixture(SemanticProfile::Luau);
    artifact.prototypes[0].constants[0] = Constant::Integer(i64::MAX);
    artifact.prototypes[0].required_features =
        FeatureBits::BASELINE | FeatureBits::INTEGER_CONSTANTS | FeatureBits::FLOOR_DIVISION;
    let artifact = ValidatedArtifact::new(artifact, limits).unwrap();
    assert_eq!(
        translate_baseline_to_luau(artifact, SemanticProfile::Luau, limits),
        Err(TranslationError::UnsupportedInstruction {
            prototype: 0,
            instruction: "floor division",
        })
    );
}

#[test]
fn floating_constants_preserve_ieee_bits_canonically() {
    let limits = BluLimits::default();
    let special = [
        f64::from_bits(0x7ff8_0000_0000_1234),
        0.0,
        -0.0,
        f64::INFINITY,
        f64::NEG_INFINITY,
        f64::from_bits(1),
    ];
    let mut artifact = fixture();
    artifact.prototypes[0]
        .constants
        .extend(special.into_iter().map(Constant::Number));
    let validated = ValidatedArtifact::new(artifact, limits).unwrap();
    let bytes = encode(&validated, limits).unwrap();
    let decoded = decode_validated(&bytes, limits).unwrap();
    let decoded_bits: Vec<_> = decoded.prototypes()[0].constants[2..]
        .iter()
        .map(|constant| match constant {
            Constant::Number(value) => value.to_bits(),
            _ => panic!("expected numeric constant"),
        })
        .collect();

    assert_eq!(decoded_bits, special.map(f64::to_bits));
    assert_eq!(decoded, validated);
    assert_eq!(encode(&decoded, limits).unwrap(), bytes);
}

#[test]
fn integer_constants_have_a_stable_tag_and_canonical_round_trip() {
    let limits = BluLimits::default();
    let values = [
        i64::MIN,
        -(1_i64 << 54),
        -(1_i64 << 53) - 1,
        -(1_i64 << 53),
        -1,
        0,
        1,
        1_i64 << 53,
        1_i64 << 54,
        i64::MAX,
    ];
    let mut artifact = baseline_fixture(SemanticProfile::Blu);
    artifact.prototypes[0].required_features =
        FeatureBits::BASELINE | FeatureBits::INTEGER_CONSTANTS;
    artifact.prototypes[0].constants = values.into_iter().map(Constant::Integer).collect();
    artifact.prototypes[0].code[0] = Instruction::LoadConstant {
        destination: 0,
        constant: 0,
    };
    artifact.prototypes[0].code[1] = Instruction::LoadConstant {
        destination: 1,
        constant: values.len() as u32 - 1,
    };
    let validated = ValidatedArtifact::new(artifact, limits).unwrap();
    let bytes = encode(&validated, limits).unwrap();
    let (first_constant, _) = first_constant_and_instruction_offsets(&bytes);

    assert_eq!(bytes[first_constant], 5);
    assert_eq!(
        &bytes[first_constant + 1..first_constant + 9],
        &i64::MIN.to_le_bytes()
    );
    assert_eq!(format!("{:?}", Constant::Integer(-7)), "Integer(-7)");

    let decoded = decode_validated(&bytes, limits).unwrap();
    assert_eq!(decoded, validated);
    assert_eq!(encode(&decoded, limits).unwrap(), bytes);
    assert_eq!(
        decoded.prototypes()[0].constants,
        values.map(Constant::Integer)
    );

    let mut mutated_tag = bytes.clone();
    mutated_tag[first_constant] = 6;
    assert!(matches!(
        decode(&mutated_tag, limits),
        Err(DecodeError::InvalidTag {
            what: "constant",
            tag: 6,
            ..
        })
    ));

    let exact_size_limits = BluLimits {
        max_bytes: bytes.len(),
        ..limits
    };
    assert!(ValidatedArtifact::new(decode(&bytes, limits).unwrap(), exact_size_limits).is_ok());
    let below_size_limits = BluLimits {
        max_bytes: bytes.len() - 1,
        ..limits
    };
    assert!(matches!(
        ValidatedArtifact::new(decode(&bytes, limits).unwrap(), below_size_limits),
        Err(ValidationError::Limit {
            what: "encoded artifact bytes",
            ..
        })
    ));

    for available_body_bytes in 0..8 {
        let end = first_constant + 1 + available_body_bytes;
        assert!(
            decode(&bytes[..end], limits).is_err(),
            "integer body prefix {available_body_bytes}"
        );
    }
}

#[test]
fn integer_storage_is_feature_gated_but_profile_neutral() {
    let limits = BluLimits::default();
    for profile in SemanticProfile::ALL {
        let mut missing = baseline_fixture(profile);
        missing.prototypes[0].constants[0] = Constant::Integer(1);
        assert!(matches!(
            ValidatedArtifact::new(missing, limits),
            Err(ValidationError::MissingFeature {
                prototype: 0,
                feature: "integer constants",
            })
        ));

        let mut declared = baseline_fixture(profile);
        declared.prototypes[0].constants[0] = Constant::Integer(1);
        declared.prototypes[0].required_features =
            FeatureBits::BASELINE | FeatureBits::INTEGER_CONSTANTS;
        assert!(ValidatedArtifact::new(declared, limits).is_ok());
    }
}

#[test]
fn blu_v1_is_not_accepted_as_a_luau_chunk() {
    let limits = BluLimits::default();
    let bytes = encode(&ValidatedArtifact::new(fixture(), limits).unwrap(), limits).unwrap();

    assert!(matches!(
        load(&bytes, LoadLimits::default()),
        Err(ChunkError::UnsupportedVersion(version)) if version == MAGIC[0]
    ));
}

#[test]
fn baseline_translation_is_explicit_validated_and_profile_preserving() {
    let limits = BluLimits::default();
    let artifact = ValidatedArtifact::new(baseline_fixture(SemanticProfile::Blu), limits).unwrap();
    let translated =
        translate_baseline_to_luau(artifact, SemanticProfile::Blu, BluLimits::default()).unwrap();
    assert_eq!(translated.profile(), SemanticProfile::Blu);
    let chunk = translated.into_validated_chunk();
    assert_eq!(chunk.semantic_profile(), Some(SemanticProfile::Blu));
    let main = &chunk.prototypes[chunk.main];

    assert_eq!(main.max_stack_size, 3);
    assert_eq!(main.instructions.len(), 4);
    assert_eq!(main.instructions[0].opcode(), Opcode::LoadK);
    assert_eq!(main.instructions[1].opcode(), Opcode::LoadK);
    assert_eq!(main.instructions[2].opcode(), Opcode::Add);
    assert_eq!(main.instructions[3].opcode(), Opcode::Return);
    assert_eq!(main.instructions[3].b(), 2);
}

#[test]
fn baseline_translation_only_converts_exact_integer_numbers() {
    let limits = BluLimits::default();
    for profile in [SemanticProfile::Blu, SemanticProfile::Luau] {
        for value in [
            0,
            -(1_i64 << 54),
            -(1_i64 << 53),
            1_i64 << 53,
            (1_i64 << 53) + 2,
            1_i64 << 54,
            i64::MIN,
        ] {
            let mut artifact = baseline_fixture(profile);
            artifact.prototypes[0].constants[0] = Constant::Integer(value);
            artifact.prototypes[0].required_features =
                FeatureBits::BASELINE | FeatureBits::INTEGER_CONSTANTS;
            let artifact = ValidatedArtifact::new(artifact, limits).unwrap();
            let translated = translate_baseline_to_luau(artifact, profile, limits).unwrap();
            assert!(matches!(
                translated.into_validated_chunk().prototypes[0].constants[0],
                LuauConstant::Number(number) if number.to_bits() == (value as f64).to_bits()
            ));
        }
    }

    for value in [-(1_i64 << 53) - 1, (1_i64 << 53) + 1, i64::MAX] {
        let mut artifact = baseline_fixture(SemanticProfile::Blu);
        artifact.prototypes[0].constants[0] = Constant::Integer(value);
        artifact.prototypes[0].required_features =
            FeatureBits::BASELINE | FeatureBits::INTEGER_CONSTANTS;
        let artifact = ValidatedArtifact::new(artifact, limits).unwrap();
        assert_eq!(
            translate_baseline_to_luau(artifact, SemanticProfile::Blu, limits),
            Err(TranslationError::IntegerNotExactlyRepresentable {
                prototype: 0,
                constant: 0,
                value,
            })
        );
    }

    let mut lua54 = baseline_fixture(SemanticProfile::Lua54);
    lua54.prototypes[0].constants[0] = Constant::Integer(1);
    lua54.prototypes[0].required_features = FeatureBits::BASELINE | FeatureBits::INTEGER_CONSTANTS;
    let lua54 = ValidatedArtifact::new(lua54, limits).unwrap();
    assert_eq!(
        translate_baseline_to_luau(lua54, SemanticProfile::Lua54, limits),
        Err(TranslationError::UnsupportedExecutionProfile(
            SemanticProfile::Lua54
        ))
    );
}

#[test]
fn baseline_translation_rejects_profile_loss_and_unrepresentable_structure() {
    let limits = BluLimits::default();
    let strict = ValidatedArtifact::new(baseline_fixture(SemanticProfile::Blu), limits).unwrap();
    let execution_limits = BluLimits {
        max_code_per_prototype: 3,
        ..limits
    };
    assert!(matches!(
        translate_baseline_to_luau(strict, SemanticProfile::Blu, execution_limits),
        Err(TranslationError::ExecutionValidation(
            ValidationError::Limit {
                what: "instruction count",
                actual: 4,
                limit: 3,
            }
        ))
    ));

    let blu = ValidatedArtifact::new(baseline_fixture(SemanticProfile::Blu), limits).unwrap();
    assert!(matches!(
        translate_baseline_to_luau(blu, SemanticProfile::Luau, limits),
        Err(TranslationError::ProfileMismatch {
            prototype: 0,
            artifact: SemanticProfile::Blu,
            execution: SemanticProfile::Luau,
        })
    ));

    let lua54 = ValidatedArtifact::new(baseline_fixture(SemanticProfile::Lua54), limits).unwrap();
    assert_eq!(
        translate_baseline_to_luau(lua54, SemanticProfile::Lua54, limits),
        Err(TranslationError::UnsupportedExecutionProfile(
            SemanticProfile::Lua54
        ))
    );

    let mut nested = fixture();
    for prototype in &mut nested.prototypes {
        prototype.profile = SemanticProfile::Blu;
    }
    let nested = ValidatedArtifact::new(nested, limits).unwrap();
    assert_eq!(
        translate_baseline_to_luau(nested, SemanticProfile::Blu, limits),
        Err(TranslationError::UnsupportedStructure {
            prototype: 0,
            what: "child prototypes",
        })
    );
}

#[test]
fn baseline_translation_checks_narrow_luau_fields() {
    let limits = BluLimits::default();
    let mut registers = baseline_fixture(SemanticProfile::Blu);
    registers.prototypes[0].register_count = 256;
    let registers = ValidatedArtifact::new(registers, limits).unwrap();
    assert!(matches!(
        translate_baseline_to_luau(registers, SemanticProfile::Blu, limits),
        Err(TranslationError::TooLarge {
            prototype: Some(0),
            what: "register count",
            actual: 256,
            limit: 255,
        })
    ));

    let mut returns = baseline_fixture(SemanticProfile::Blu);
    returns.prototypes[0].register_count = 255;
    returns.prototypes[0].code = (0..255)
        .map(|destination| Instruction::LoadConstant {
            destination,
            constant: 0,
        })
        .collect();
    returns.prototypes[0].code.push(Instruction::Return {
        first: 0,
        count: 255,
    });
    let span = returns.prototypes[0].source_map[0];
    returns.prototypes[0].source_map = vec![span; 256];
    let returns = ValidatedArtifact::new(returns, limits).unwrap();
    assert!(matches!(
        translate_baseline_to_luau(returns, SemanticProfile::Blu, limits),
        Err(TranslationError::TooLarge {
            prototype: Some(0),
            what: "fixed return count",
            actual: 255,
            limit: 254,
        })
    ));
}

#[test]
fn every_truncation_is_rejected_without_panicking() {
    let limits = BluLimits::default();
    let bytes = encode(&ValidatedArtifact::new(fixture(), limits).unwrap(), limits).unwrap();

    for end in 0..bytes.len() {
        assert!(decode(&bytes[..end], limits).is_err(), "prefix {end}");
    }
}

#[test]
fn declared_collection_bodies_are_preflighted_before_reservation() {
    let limits = BluLimits::default();
    let bytes = encode(&ValidatedArtifact::new(fixture(), limits).unwrap(), limits).unwrap();

    let after_source_count = source_count_offset(&bytes) + 4;
    assert!(matches!(
        decode(&bytes[..after_source_count], limits),
        Err(DecodeError::DeclaredBodyTooLarge {
            what: "source count",
            count: 1,
            minimum_width: 44,
            remaining: 0,
            ..
        })
    ));

    let after_prototype_count = first_prototype_offset(&bytes) - 4;
    assert!(matches!(
        decode(&bytes[..after_prototype_count], limits),
        Err(DecodeError::DeclaredBodyTooLarge {
            what: "prototype count",
            count: 2,
            minimum_width: 48,
            remaining: 0,
            ..
        })
    ));
}

#[test]
fn serialized_discriminants_reserved_fields_and_trailing_data_are_rejected() {
    let limits = BluLimits::default();
    let bytes = encode(&ValidatedArtifact::new(fixture(), limits).unwrap(), limits).unwrap();

    let mut bad_magic = bytes.clone();
    bad_magic[0] ^= 0x80;
    assert!(matches!(
        decode(&bad_magic, limits),
        Err(DecodeError::InvalidMagic(_))
    ));

    let mut bad_version = bytes.clone();
    bad_version[4..6].copy_from_slice(&(BLU_V1_VERSION + 1).to_le_bytes());
    assert!(matches!(
        decode(&bad_version, limits),
        Err(DecodeError::UnsupportedVersion(_))
    ));

    let mut reserved = bytes.clone();
    reserved[6] = 1;
    assert!(matches!(
        decode(&reserved, limits),
        Err(DecodeError::UnsupportedField {
            what: "header flags",
            ..
        })
    ));

    let prototype = first_prototype_offset(&bytes);
    let (constant, instruction) = first_constant_and_instruction_offsets(&bytes);

    let mut reserved_profile = bytes.clone();
    reserved_profile[prototype] = 0;
    assert!(matches!(
        decode(&reserved_profile, limits),
        Err(DecodeError::InvalidTag {
            what: "semantic profile",
            tag: 0,
            ..
        })
    ));

    let mut unknown_profile = bytes.clone();
    unknown_profile[prototype] = 8;
    assert!(matches!(
        decode(&unknown_profile, limits),
        Err(DecodeError::InvalidTag {
            what: "semantic profile",
            tag: 8,
            ..
        })
    ));

    let mut bad_vararg = bytes.clone();
    bad_vararg[prototype + 1] = 2;
    assert!(matches!(
        decode(&bad_vararg, limits),
        Err(DecodeError::InvalidTag {
            what: "vararg",
            tag: 2,
            ..
        })
    ));

    let mut prototype_reserved = bytes.clone();
    prototype_reserved[prototype + 2] = 1;
    assert!(matches!(
        decode(&prototype_reserved, limits),
        Err(DecodeError::UnsupportedField {
            what: "prototype flags",
            ..
        })
    ));

    let mut bad_constant = bytes.clone();
    bad_constant[constant] = u8::MAX;
    assert!(matches!(
        decode(&bad_constant, limits),
        Err(DecodeError::InvalidTag {
            what: "constant",
            tag: u8::MAX,
            ..
        })
    ));

    let mut bad_instruction = bytes.clone();
    bad_instruction[instruction] = u8::MAX;
    assert!(matches!(
        decode(&bad_instruction, limits),
        Err(DecodeError::InvalidTag {
            what: "instruction",
            tag: u8::MAX,
            ..
        })
    ));

    let mut trailing = bytes;
    trailing.push(0);
    assert_eq!(
        decode(&trailing, limits),
        Err(DecodeError::TrailingBytes { count: 1 })
    );
}

#[test]
fn validator_rejects_mutated_features_flow_maps_and_tree() {
    let limits = BluLimits::default();

    let mut artifact = fixture();
    artifact.prototypes[0].required_features = FeatureBits::from_bits(1 << 63);
    assert!(matches!(
        ValidatedArtifact::new(artifact, limits),
        Err(ValidationError::UnsupportedFeatures { .. })
    ));

    let mut artifact = fixture();
    artifact.prototypes[0].code[2] = Instruction::Add {
        destination: 2,
        left: 0,
        right: 2,
    };
    assert!(matches!(
        ValidatedArtifact::new(artifact, limits),
        Err(ValidationError::InvalidInstruction {
            what: "register is read before initialization",
            ..
        })
    ));

    let mut artifact = fixture();
    artifact.prototypes[0].source_map.pop();
    assert!(matches!(
        ValidatedArtifact::new(artifact, limits),
        Err(ValidationError::InvalidSourceMap { .. })
    ));

    let mut artifact = fixture();
    artifact.prototypes[0].children = vec![];
    artifact.prototypes[1].children = vec![1];
    assert!(matches!(
        ValidatedArtifact::new(artifact, limits),
        Err(ValidationError::InvalidPrototypeTree { .. })
    ));
}

#[test]
fn validator_detects_a_cycle_disconnected_from_main() {
    let limits = BluLimits::default();
    let mut artifact = fixture();
    let third = fixture().prototypes.remove(1);
    artifact.prototypes.push(third);
    artifact.prototypes[0].children.clear();
    artifact.prototypes[1].children = vec![2];
    artifact.prototypes[2].children = vec![1];

    assert!(matches!(
        ValidatedArtifact::new(artifact, limits),
        Err(ValidationError::InvalidPrototypeTree {
            message: "prototype graph contains a cycle",
            ..
        })
    ));
}

#[test]
fn validator_enforces_tree_and_source_map_boundaries() {
    let limits = BluLimits::default();
    let source = SourceId::new(7);

    let mut at_source_end = fixture();
    at_source_end.prototypes[0].source_map[3] = ByteSpan::from_usize(source, 80, 80).unwrap();
    assert!(ValidatedArtifact::new(at_source_end, limits).is_ok());

    let mut past_source_end = fixture();
    past_source_end.prototypes[0].source_map[3] = ByteSpan::from_usize(source, 80, 81).unwrap();
    assert!(matches!(
        ValidatedArtifact::new(past_source_end, limits),
        Err(ValidationError::InvalidSourceMap {
            what: "span exceeds source length",
            ..
        })
    ));

    let mut foreign_source = fixture();
    foreign_source.prototypes[0].source_map[0] =
        ByteSpan::from_usize(SourceId::new(8), 0, 1).unwrap();
    assert!(matches!(
        ValidatedArtifact::new(foreign_source, limits),
        Err(ValidationError::InvalidSourceMap {
            what: "span belongs to a different source",
            ..
        })
    ));

    let mut child_at_count = fixture();
    child_at_count.prototypes[0].children[0] = 2;
    assert!(matches!(
        ValidatedArtifact::new(child_at_count, limits),
        Err(ValidationError::InvalidReference {
            what: "child prototype",
            index: 2,
            count: 2,
            ..
        })
    ));

    let mut main_at_count = fixture();
    main_at_count.main = 2;
    assert!(matches!(
        ValidatedArtifact::new(main_at_count, limits),
        Err(ValidationError::InvalidReference {
            what: "main prototype",
            index: 2,
            count: 2,
            ..
        })
    ));
}

#[test]
fn decode_limits_counts_before_allocation() {
    let limits = BluLimits {
        max_sources: 0,
        ..BluLimits::default()
    };
    let bytes = encode(
        &ValidatedArtifact::new(fixture(), BluLimits::default()).unwrap(),
        BluLimits::default(),
    )
    .unwrap();
    assert!(matches!(
        decode(&bytes, limits),
        Err(DecodeError::TooLarge {
            what: "source count",
            ..
        })
    ));
}

#[test]
fn validation_enforces_the_total_encoded_size_limit() {
    let limits = BluLimits {
        max_bytes: 16,
        ..BluLimits::default()
    };
    assert!(matches!(
        ValidatedArtifact::new(fixture(), limits),
        Err(ValidationError::Limit {
            what: "encoded artifact bytes",
            ..
        })
    ));
}

#[test]
fn validation_enforces_aggregate_work_limits_for_host_built_artifacts() {
    let cases: [LimitCase; 10] = [
        (
            |limits| limits.max_total_registers = 3,
            "total register count",
        ),
        (
            |limits| limits.max_total_constants = 4,
            "total constant count",
        ),
        (
            |limits| limits.max_total_constant_bytes = 0,
            "total constant bytes",
        ),
        (
            |limits| limits.max_total_code = 4,
            "total instruction count",
        ),
        (|limits| limits.max_total_children = 0, "total child count"),
        (
            |limits| limits.max_total_upvalues = 0,
            "total upvalue count",
        ),
        (
            |limits| limits.max_total_debug_entries = 1,
            "total debug entry count",
        ),
        (
            |limits| limits.max_total_debug_bytes = 13,
            "total debug bytes",
        ),
        (
            |limits| limits.max_total_source_map_entries = 4,
            "total source map entry count",
        ),
        (
            |limits| limits.max_decoded_bytes = 0,
            "estimated decoded bytes",
        ),
    ];

    for (configure, expected) in cases {
        let mut limits = BluLimits::default();
        configure(&mut limits);
        assert!(
            matches!(
                ValidatedArtifact::new(fixture(), limits),
                Err(ValidationError::Limit { what, .. }) if what == expected
            ),
            "{expected}"
        );
    }
}

#[test]
fn debug_names_have_an_independent_per_name_limit() {
    let constant_only = BluLimits {
        max_constant_bytes: 1,
        ..BluLimits::default()
    };
    let validated = ValidatedArtifact::new(fixture(), constant_only).unwrap();
    let bytes = encode(&validated, constant_only).unwrap();
    assert!(decode_validated(&bytes, constant_only).is_ok());

    let debug_limited = BluLimits {
        max_constant_bytes: 1,
        max_debug_name_bytes: 7,
        ..BluLimits::default()
    };
    assert!(matches!(
        ValidatedArtifact::new(fixture(), debug_limited),
        Err(ValidationError::Limit {
            what: "debug name bytes",
            actual: 8,
            limit: 7,
        })
    ));
    assert!(matches!(
        decode(&bytes, debug_limited),
        Err(DecodeError::TooLarge {
            what: "upvalue name",
            actual: 8,
            limit: 7,
        })
    ));
}

#[test]
fn decoder_consumes_aggregate_budgets_before_growing_collections() {
    let defaults = BluLimits::default();
    let bytes = encode(
        &ValidatedArtifact::new(fixture(), defaults).unwrap(),
        defaults,
    )
    .unwrap();

    let register_limits = BluLimits {
        max_total_registers: 3,
        ..defaults
    };
    assert!(matches!(
        decode(&bytes, register_limits),
        Err(DecodeError::TooLarge {
            what: "total register count",
            actual: 4,
            limit: 3,
        })
    ));

    let code_limits = BluLimits {
        max_total_code: 4,
        ..defaults
    };
    assert!(matches!(
        decode(&bytes, code_limits),
        Err(DecodeError::TooLarge {
            what: "total instruction count",
            actual: 5,
            limit: 4,
        })
    ));

    let byte_limits = BluLimits {
        max_total_constant_bytes: 0,
        ..defaults
    };
    assert!(matches!(
        decode(&bytes, byte_limits),
        Err(DecodeError::TooLarge {
            what: "total constant bytes",
            actual: 1,
            limit: 0,
        })
    ));

    let debug_limits = BluLimits {
        max_total_debug_bytes: 13,
        ..defaults
    };
    assert!(matches!(
        decode(&bytes, debug_limits),
        Err(DecodeError::TooLarge {
            what: "total debug bytes",
            actual: 14,
            limit: 13,
        })
    ));

    // Golden BluV1 logical accounting for `fixture`; this must not vary with
    // Rust layout, compiler, allocator, or target architecture.
    const FIXTURE_DECODED_BYTES: usize = 1_281;
    let exact_decoded_limits = BluLimits {
        max_decoded_bytes: FIXTURE_DECODED_BYTES,
        ..defaults
    };
    assert!(ValidatedArtifact::new(fixture(), exact_decoded_limits).is_ok());
    assert!(decode_validated(&bytes, exact_decoded_limits).is_ok());

    let below_decoded_limits = BluLimits {
        max_decoded_bytes: FIXTURE_DECODED_BYTES - 1,
        ..defaults
    };
    assert!(matches!(
        ValidatedArtifact::new(fixture(), below_decoded_limits),
        Err(ValidationError::Limit {
            what: "estimated decoded bytes",
            actual: FIXTURE_DECODED_BYTES,
            limit: 1_280,
        })
    ));
    assert!(matches!(
        decode(&bytes, below_decoded_limits),
        Err(DecodeError::TooLarge {
            what: "estimated decoded bytes",
            actual: FIXTURE_DECODED_BYTES,
            limit: 1_280,
        })
    ));
}
