use crate::{Chunk, Constant, Instruction, Prototype};
use core::fmt::Write;

#[must_use]
pub fn disassemble(chunk: &Chunk) -> String {
    let mut output = String::new();
    let _ = writeln!(
        output,
        "; Luau bytecode v{} typeinfo v{} main p{}",
        chunk.version, chunk.typeinfo_version, chunk.main
    );
    for (index, prototype) in chunk.prototypes.iter().enumerate() {
        let name = prototype
            .debug_name
            .and_then(|name| chunk.strings.get(name))
            .map(|name| String::from_utf8_lossy(name))
            .unwrap_or_else(|| "<anonymous>".into());
        let _ = writeln!(
            output,
            "\np{index} {name} (params={}, stack={}, upvalues={}, vararg={})",
            prototype.parameter_count,
            prototype.max_stack_size,
            prototype.upvalue_count,
            prototype.is_vararg
        );
        for instruction in &prototype.instructions {
            write_instruction(&mut output, prototype, *instruction);
        }
    }
    output
}

fn write_instruction(output: &mut String, prototype: &Prototype, instruction: Instruction) {
    let _ = write!(
        output,
        "{:04}  {:<18} A={} B={} C={} D={}",
        instruction.pc(),
        instruction.opcode(),
        instruction.a(),
        instruction.b(),
        instruction.c(),
        instruction.d()
    );
    if let Some(aux) = instruction.aux() {
        let _ = write!(output, " AUX={aux}");
    }
    if let Some(target) = instruction.jump_target() {
        let _ = write!(output, " -> {target}");
    }
    if let Some(constant) = referenced_constant(prototype, instruction) {
        let _ = write!(
            output,
            " ; k{constant}={}",
            display_constant(prototype, constant)
        );
    }
    output.push('\n');
}

fn referenced_constant(prototype: &Prototype, instruction: Instruction) -> Option<usize> {
    use crate::Opcode::*;
    let index = match instruction.opcode() {
        LoadK | DupTable | DupClosure => usize::try_from(instruction.d()).ok()?,
        LoadKx | GetGlobal | SetGlobal | GetTableKs | SetTableKs | NameCall | NewClassMember => {
            instruction.aux()? as usize
        }
        AddK | SubK | MulK | DivK | ModK | PowK | AndK | OrK | IDivK => instruction.c() as usize,
        SubRk | DivRk => instruction.b() as usize,
        JumpXEqKN | JumpXEqKS => (instruction.aux()? & 0x00ff_ffff) as usize,
        GetUdataKs | SetUdataKs | NameCallUdata => (instruction.aux()? & 0xffff) as usize,
        _ => return None,
    };
    prototype.constants.get(index)?;
    Some(index)
}

fn display_constant(prototype: &Prototype, index: usize) -> String {
    match &prototype.constants[index] {
        Constant::Nil => "nil".into(),
        Constant::Boolean(value) => value.to_string(),
        Constant::Number(value) => value.to_string(),
        Constant::Integer(value) => value.to_string(),
        Constant::String(value) => format!("string#{value}"),
        Constant::Import(value) => format!("import({value:#010x})"),
        Constant::Vector(value) => format!("{value:?}"),
        Constant::VectorDouble(value) => format!("{value:?}"),
        Constant::Table(keys) => format!("table{keys:?}"),
        Constant::TableWithConstants(entries) => format!("table{entries:?}"),
        Constant::Closure(prototype) => format!("closure(p{prototype})"),
        Constant::ClassShape {
            class_name,
            properties,
            methods,
        } => format!("class({class_name}; {properties:?}; {methods:?})"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{DebugInfo, FeedbackSlot, LineInfo, Opcode};

    #[test]
    fn emits_stable_prototype_and_instruction_listing() {
        let code = vec![
            u32::from(Opcode::LoadK as u8),
            u32::from(Opcode::Return as u8) | (1 << 16),
        ];
        let chunk = Chunk {
            version: 12,
            typeinfo_version: 3,
            strings: Vec::new(),
            userdata_types: Vec::new(),
            prototypes: vec![Prototype {
                max_stack_size: 1,
                parameter_count: 0,
                upvalue_count: 0,
                is_vararg: false,
                flags: 0,
                typeinfo: Vec::new(),
                instructions: crate::decode(&code).unwrap(),
                code,
                constants: vec![Constant::Number(3.0)],
                children: Vec::new(),
                line_defined: 0,
                debug_name: None,
                line_info: None::<LineInfo>,
                debug_info: None::<DebugInfo>,
                feedback: Vec::<FeedbackSlot>::new(),
                cost: None,
            }],
            main: 0,
        };
        let output = disassemble(&chunk);
        assert!(output.contains("main p0"));
        assert!(output.contains("0000  LoadK"));
        assert!(output.contains("k0=3"));
    }
}
