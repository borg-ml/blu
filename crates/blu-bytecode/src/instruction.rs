// Ported from Luau Bytecode.h and BytecodeUtils.h at the pinned revision.
// Luau is copyright Roblox Corporation and Lua.org, PUC-Rio, MIT licensed.

use crate::Opcode;
use core::fmt;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Instruction {
    pc: usize,
    word: u32,
    opcode: Opcode,
    aux: Option<u32>,
}

impl Instruction {
    #[must_use]
    pub const fn pc(self) -> usize {
        self.pc
    }

    #[must_use]
    pub const fn word(self) -> u32 {
        self.word
    }

    #[must_use]
    pub const fn opcode(self) -> Opcode {
        self.opcode
    }

    #[must_use]
    pub const fn aux(self) -> Option<u32> {
        self.aux
    }

    #[must_use]
    pub const fn a(self) -> u8 {
        ((self.word >> 8) & 0xff) as u8
    }

    #[must_use]
    pub const fn b(self) -> u8 {
        ((self.word >> 16) & 0xff) as u8
    }

    #[must_use]
    pub const fn c(self) -> u8 {
        ((self.word >> 24) & 0xff) as u8
    }

    #[must_use]
    pub const fn d(self) -> i16 {
        (self.word >> 16) as i16
    }

    #[must_use]
    pub const fn e(self) -> i32 {
        (self.word as i32) >> 8
    }

    #[must_use]
    pub const fn aux_a(self) -> Option<u8> {
        match self.aux {
            Some(aux) => Some((aux & 0xff) as u8),
            None => None,
        }
    }

    #[must_use]
    pub const fn aux_b(self) -> Option<u8> {
        match self.aux {
            Some(aux) => Some(((aux >> 8) & 0xff) as u8),
            None => None,
        }
    }

    #[must_use]
    pub fn jump_target(self) -> Option<usize> {
        let delta = if self.opcode.uses_d_jump() {
            i64::from(self.d()) + 1
        } else if self.opcode.is_fast_call() {
            i64::from(self.c()) + 2
        } else if self.opcode == Opcode::LoadB && self.c() != 0 {
            i64::from(self.c()) + 1
        } else if self.opcode == Opcode::JumpX {
            i64::from(self.e()) + 1
        } else {
            return None;
        };
        let target = i64::try_from(self.pc).ok()?.checked_add(delta)?;
        usize::try_from(target).ok()
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DecodeError {
    InvalidOpcode { pc: usize, opcode: u8 },
    MissingAux { pc: usize, opcode: Opcode },
    Allocation { requested: usize },
}

impl fmt::Display for DecodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidOpcode { pc, opcode } => {
                write!(f, "invalid opcode {opcode} at word {pc}")
            }
            Self::MissingAux { pc, opcode } => {
                write!(f, "missing auxiliary word for {opcode} at word {pc}")
            }
            Self::Allocation { requested } => {
                write!(
                    f,
                    "failed to allocate space for {requested} decoded instructions"
                )
            }
        }
    }
}

impl std::error::Error for DecodeError {}

pub struct InstructionIter<'a> {
    words: &'a [u32],
    pc: usize,
}

impl<'a> InstructionIter<'a> {
    #[must_use]
    pub const fn new(words: &'a [u32]) -> Self {
        Self { words, pc: 0 }
    }
}

impl Iterator for InstructionIter<'_> {
    type Item = Result<Instruction, DecodeError>;

    fn next(&mut self) -> Option<Self::Item> {
        let word = *self.words.get(self.pc)?;
        let pc = self.pc;
        let raw_opcode = (word & 0xff) as u8;
        let opcode = match Opcode::try_from(raw_opcode) {
            Ok(opcode) => opcode,
            Err(opcode) => {
                self.pc = self.words.len();
                return Some(Err(DecodeError::InvalidOpcode { pc, opcode }));
            }
        };
        let aux = if opcode.words() == 2 {
            match self.words.get(pc + 1).copied() {
                Some(aux) => Some(aux),
                None => {
                    self.pc = self.words.len();
                    return Some(Err(DecodeError::MissingAux { pc, opcode }));
                }
            }
        } else {
            None
        };
        self.pc += usize::from(opcode.words());
        Some(Ok(Instruction {
            pc,
            word,
            opcode,
            aux,
        }))
    }
}

pub fn decode(words: &[u32]) -> Result<Vec<Instruction>, DecodeError> {
    let mut instructions = Vec::new();
    instructions
        .try_reserve_exact(words.len())
        .map_err(|_| DecodeError::Allocation {
            requested: words.len(),
        })?;
    for instruction in InstructionIter::new(words) {
        instructions.push(instruction?);
    }
    Ok(instructions)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn abc(opcode: Opcode, a: u8, b: u8, c: u8) -> u32 {
        u32::from(opcode as u8) | (u32::from(a) << 8) | (u32::from(b) << 16) | (u32::from(c) << 24)
    }

    fn ad(opcode: Opcode, a: u8, d: i16) -> u32 {
        u32::from(opcode as u8) | (u32::from(a) << 8) | ((d as u16 as u32) << 16)
    }

    #[test]
    fn decodes_abc_ad_and_auxiliary_words() {
        let words = [
            abc(Opcode::Add, 2, 3, 4),
            ad(Opcode::GetGlobal, 7, -2),
            0x1234_5678,
        ];
        let instructions = decode(&words).unwrap();
        assert_eq!(instructions.len(), 2);
        assert_eq!(instructions[0].opcode(), Opcode::Add);
        assert_eq!(
            (
                instructions[0].a(),
                instructions[0].b(),
                instructions[0].c()
            ),
            (2, 3, 4)
        );
        assert_eq!(instructions[1].pc(), 1);
        assert_eq!(instructions[1].d(), -2);
        assert_eq!(instructions[1].aux(), Some(0x1234_5678));
    }

    #[test]
    fn rejects_unknown_opcode_and_truncated_auxiliary_word() {
        assert_eq!(
            decode(&[Opcode::COUNT.into()]),
            Err(DecodeError::InvalidOpcode {
                pc: 0,
                opcode: Opcode::COUNT
            })
        );
        assert_eq!(
            decode(&[Opcode::GetGlobal as u32]),
            Err(DecodeError::MissingAux {
                pc: 0,
                opcode: Opcode::GetGlobal
            })
        );
    }

    #[test]
    fn computes_signed_jump_targets_in_word_coordinates() {
        let forward = decode(&[ad(Opcode::Jump, 0, 2)]).unwrap()[0];
        assert_eq!(forward.jump_target(), Some(3));

        let backward = Instruction {
            pc: 4,
            word: ad(Opcode::JumpBack, 0, -3),
            opcode: Opcode::JumpBack,
            aux: None,
        };
        assert_eq!(backward.jump_target(), Some(2));
    }
}
