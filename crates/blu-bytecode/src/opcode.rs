// Ported from Luau Bytecode.h and BytecodeUtils.h at the pinned revision.
// Luau is copyright Roblox Corporation and Lua.org, PUC-Rio, MIT licensed.

use core::fmt;

macro_rules! opcodes {
    ($($name:ident = $value:literal),+ $(,)?) => {
        #[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
        #[repr(u8)]
        pub enum Opcode {
            $($name = $value),+
        }

        impl TryFrom<u8> for Opcode {
            type Error = u8;

            fn try_from(value: u8) -> Result<Self, Self::Error> {
                match value {
                    $($value => Ok(Self::$name),)+
                    other => Err(other),
                }
            }
        }
    };
}

opcodes! {
    Nop = 0,
    Break = 1,
    LoadNil = 2,
    LoadB = 3,
    LoadN = 4,
    LoadK = 5,
    Move = 6,
    GetGlobal = 7,
    SetGlobal = 8,
    GetUpval = 9,
    SetUpval = 10,
    CloseUpvals = 11,
    GetImport = 12,
    GetTable = 13,
    SetTable = 14,
    GetTableKs = 15,
    SetTableKs = 16,
    GetTableN = 17,
    SetTableN = 18,
    NewClosure = 19,
    NameCall = 20,
    Call = 21,
    Return = 22,
    Jump = 23,
    JumpBack = 24,
    JumpIf = 25,
    JumpIfNot = 26,
    JumpIfEq = 27,
    JumpIfLe = 28,
    JumpIfLt = 29,
    JumpIfNotEq = 30,
    JumpIfNotLe = 31,
    JumpIfNotLt = 32,
    Add = 33,
    Sub = 34,
    Mul = 35,
    Div = 36,
    Mod = 37,
    Pow = 38,
    AddK = 39,
    SubK = 40,
    MulK = 41,
    DivK = 42,
    ModK = 43,
    PowK = 44,
    And = 45,
    Or = 46,
    AndK = 47,
    OrK = 48,
    Concat = 49,
    Not = 50,
    Minus = 51,
    Length = 52,
    NewTable = 53,
    DupTable = 54,
    SetList = 55,
    ForNPrep = 56,
    ForNLoop = 57,
    ForGLoop = 58,
    ForGPrepInext = 59,
    FastCall3 = 60,
    ForGPrepNext = 61,
    NativeCall = 62,
    GetVarargs = 63,
    DupClosure = 64,
    PrepVarargs = 65,
    LoadKx = 66,
    JumpX = 67,
    FastCall = 68,
    Coverage = 69,
    Capture = 70,
    SubRk = 71,
    DivRk = 72,
    FastCall1 = 73,
    FastCall2 = 74,
    FastCall2K = 75,
    ForGPrep = 76,
    JumpXEqKNil = 77,
    JumpXEqKB = 78,
    JumpXEqKN = 79,
    JumpXEqKS = 80,
    IDiv = 81,
    IDivK = 82,
    GetUdataKs = 83,
    SetUdataKs = 84,
    NameCallUdata = 85,
    NewClassMember = 86,
    CallFb = 87,
    CmpProto = 88,
}

impl Opcode {
    pub const COUNT: u8 = 89;

    #[must_use]
    pub const fn words(self) -> u8 {
        match self {
            Self::GetGlobal
            | Self::SetGlobal
            | Self::GetImport
            | Self::GetTableKs
            | Self::SetTableKs
            | Self::NameCall
            | Self::JumpIfEq
            | Self::JumpIfLe
            | Self::JumpIfLt
            | Self::JumpIfNotEq
            | Self::JumpIfNotLe
            | Self::JumpIfNotLt
            | Self::NewTable
            | Self::SetList
            | Self::ForGLoop
            | Self::LoadKx
            | Self::FastCall2
            | Self::FastCall2K
            | Self::FastCall3
            | Self::JumpXEqKNil
            | Self::JumpXEqKB
            | Self::JumpXEqKN
            | Self::JumpXEqKS
            | Self::GetUdataKs
            | Self::SetUdataKs
            | Self::NameCallUdata
            | Self::NewClassMember
            | Self::CallFb
            | Self::CmpProto => 2,
            _ => 1,
        }
    }

    #[must_use]
    pub const fn is_fast_call(self) -> bool {
        matches!(
            self,
            Self::FastCall | Self::FastCall1 | Self::FastCall2 | Self::FastCall2K | Self::FastCall3
        )
    }

    #[must_use]
    pub const fn uses_d_jump(self) -> bool {
        matches!(
            self,
            Self::Jump
                | Self::JumpIf
                | Self::JumpIfNot
                | Self::JumpIfEq
                | Self::JumpIfLe
                | Self::JumpIfLt
                | Self::JumpIfNotEq
                | Self::JumpIfNotLe
                | Self::JumpIfNotLt
                | Self::ForNPrep
                | Self::ForNLoop
                | Self::ForGPrep
                | Self::ForGLoop
                | Self::ForGPrepInext
                | Self::ForGPrepNext
                | Self::JumpBack
                | Self::JumpXEqKNil
                | Self::JumpXEqKB
                | Self::JumpXEqKN
                | Self::JumpXEqKS
                | Self::CmpProto
        )
    }
}

impl fmt::Display for Opcode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{self:?}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn opcode_numbers_match_pinned_luau_order() {
        assert_eq!(Opcode::Nop as u8, 0);
        assert_eq!(Opcode::FastCall3 as u8, 60);
        assert_eq!(Opcode::CmpProto as u8, 88);
        assert_eq!(Opcode::COUNT, 89);
        for raw in 0..Opcode::COUNT {
            assert_eq!(Opcode::try_from(raw).unwrap() as u8, raw);
        }
        assert_eq!(Opcode::try_from(Opcode::COUNT), Err(Opcode::COUNT));
    }

    #[test]
    fn auxiliary_word_lengths_match_upstream() {
        assert_eq!(Opcode::LoadK.words(), 1);
        assert_eq!(Opcode::GetGlobal.words(), 2);
        assert_eq!(Opcode::FastCall3.words(), 2);
        assert_eq!(Opcode::CmpProto.words(), 2);
    }
}
