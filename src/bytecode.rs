//! Bytecode representation and VM instruction set.
//!
//! This module defines the bytecode format used by the VM, including:
//! - [`Op`] enum: All VM opcodes with safe conversions to/from `u8`
//! - [`Chunk`] struct: Container for bytecode, constants, and function prototypes
//! - [`FunctionProto`] struct: Metadata for compiled functions
//!
//! # Instruction Format
//!
//! Jump instructions use a 3-byte encoding:
//! - Byte 0: Opcode
//! - Bytes 1-2: Signed 16-bit little-endian offset (relative to next instruction)
//!
//! # Hotpatching
//!
//! The [`Chunk`] type provides helpers for runtime bytecode modification:
//! - [`Chunk::hotwire_jump_to_next`] - Convert jump to no-op
//! - [`Chunk::hotwire_make_unconditional`] - Make conditional jump unconditional
//! - [`Chunk::hotwire_redirect`] - Change jump target
//!
//! Safety: Hotpatching requires exclusive access to the bytecode. Callers must
//! ensure no concurrent execution while modifying chunks.
//!
//! # Example
//!
//! ```
//! use tsblitz::bytecode::{Chunk, Op};
//!
//! let mut chunk = Chunk::new();
//! chunk.write(Op::Const as u8, 1);
//! chunk.write(0, 1); // Constant index
//! ```

use crate::typecheck::Type;
use crate::value::Value;
use std::convert::TryFrom;
use std::rc::Rc;

#[derive(Clone, Debug)]
/// Description of a captured binding for a closure.
pub struct UpvalueSpec {
    /// True when capturing from the immediately enclosing function's locals;
    /// false when capturing from the enclosing function's own upvalue list.
    pub is_local: bool,
    /// Slot or upvalue index in the enclosing function.
    pub index: u8,
}

#[derive(Clone, Debug)]
/// A chunk of bytecode containing opcodes, constants, and metadata
pub struct Chunk {
    /// Bytecode instructions
    pub code: Vec<u8>,
    /// Constant pool values
    pub constants: Vec<Value>,
    /// Function prototypes
    pub functions: Vec<FunctionProto>,
    /// Line number information for debugging
    pub lines: Vec<u32>,
}

#[derive(Clone, Debug)]
/// Function prototype containing metadata for compiled functions
pub struct FunctionProto {
    /// Function name
    pub name: String,
    /// Number of parameters
    pub arity: usize,
    /// Compiled bytecode for this function
    pub chunk: Rc<Chunk>,
    /// NOTE: slot type annotations have been removed in JS-first mode; the
    /// runtime no longer enforces TypeScript annotations.
    pub slot_types: Vec<Option<Type>>,
    /// Captured binding descriptors for nested closures.
    pub upvalues: Vec<UpvalueSpec>,
    /// Whether the last parameter is a rest parameter (`...args`). If true,
    /// at runtime the last local will be populated with an array of the
    /// remaining arguments.
    pub has_rest: bool,
    /// Whether creating this function should install an own `.prototype`
    /// object, i.e. whether it behaves like a constructor function.
    pub has_prototype_property: bool,
    /// Whether the function should capture lexical `this` at closure creation.
    pub captures_this: bool,
    /// Slot index for the implicit `arguments` binding, if reserved.
    pub arguments_slot: Option<usize>,
    /// Return type annotation
    pub return_type: Option<Type>,
}

impl Chunk {
    /// Creates a new empty chunk
    pub fn new() -> Self {
        Self {
            code: Vec::new(),
            constants: Vec::new(),
            functions: Vec::new(),
            lines: Vec::new(),
        }
    }

    /// Writes a byte to the chunk with line number information
    pub fn write(&mut self, byte: u8, line: u32) {
        self.code.push(byte);
        self.lines.push(line);
    }

    /// Adds a constant to the constant pool and returns its index
    pub fn add_constant(&mut self, value: Value) -> usize {
        self.constants.push(value);
        self.constants.len() - 1
    }

    /// Adds a function prototype to the function list and returns its index
    pub fn add_function(&mut self, proto: FunctionProto) -> usize {
        self.functions.push(proto);
        self.functions.len() - 1
    }

    // --- Helpers for reading/writing 16-bit operands (little-endian) ---

    /// Reads a signed 16-bit little-endian value from `code` at `pos`.
    /// Returns `None` if the two bytes are out of bounds.
    pub fn read_i16_at(&self, pos: usize) -> Option<i16> {
        if pos + 1 >= self.code.len() {
            return None;
        }
        let lo = self.code[pos] as u16;
        let hi = self.code[pos + 1] as u16;
        let val = ((hi << 8) | lo) as i16;
        Some(val)
    }

    /// Writes a signed 16-bit little-endian value into `code` at `pos`.
    /// Returns `false` if out of bounds.
    pub fn write_i16_at(&mut self, pos: usize, val: i16) -> bool {
        if pos + 1 >= self.code.len() {
            return false;
        }
        let bytes = val.to_le_bytes();
        self.code[pos] = bytes[0];
        self.code[pos + 1] = bytes[1];
        true
    }

    /// Helper: returns the instruction index of the jump target for a jump located at `ip`.
    /// `ip` is the index of the opcode byte. Returns `None` if `ip` is out of bounds,
    /// the opcode at `ip` is not a jump, or the computed target is out of bounds.
    pub fn jump_target_from_ip(&self, ip: usize) -> Option<usize> {
        // need opcode + 2 bytes for i16 operand
        if ip + 1 + 1 >= self.code.len() {
            return None;
        }
        let b = self.code[ip];
        let op = Op::from_byte(b)?;
        if !op.is_jump() {
            return None;
        }
        // operand starts at ip + 1
        let offset = self.read_i16_at(ip + 1)? as i32;
        let next_instr = ip + 1 + 2;
        // offset is relative to the next instruction
        let target = (next_instr as i32).checked_add(offset)?;
        if target < 0 {
            return None;
        }
        let t = target as usize;
        if t >= self.code.len() {
            return None;
        }
        Some(t)
    }

    /// Hotwire the jump at `ip` so it jumps to the following instruction (i.e. becomes a no-op jump).
    /// Returns the original `Op` if successful or `None` on error (non-jump opcode, OOB, etc).
    ///
    /// Safety: callers must ensure no concurrent execution is reading/executing this `Chunk`.
    pub fn hotwire_jump_to_next(&mut self, ip: usize) -> Option<Op> {
        // verify opcode + operand is present
        if ip + 1 + 1 >= self.code.len() {
            return None;
        }
        let byte = self.code[ip];
        let op = Op::from_byte(byte)?;
        if !op.is_jump() {
            return None;
        }
        // set relative offset to 0 so target == next instruction
        let ok = self.write_i16_at(ip + 1, 0);
        if !ok {
            return None;
        }
        Some(op)
    }

    /// Convert `JumpIfFalse` at `ip` to unconditional `Jump`.
    /// Returns the original `Op` if successful or `None` on error.
    ///
    /// This changes only the opcode byte; the operand layout must be identical
    /// between the two opcodes (same size and semantics).
    pub fn hotwire_make_unconditional(&mut self, ip: usize) -> Option<Op> {
        if ip >= self.code.len() {
            return None;
        }
        let byte = self.code[ip];
        let op = Op::from_byte(byte)?;
        match op {
            Op::JumpIfFalse => {
                self.code[ip] = Op::Jump.to_byte();
                Some(op)
            }
            Op::Jump => Some(op), // already unconditional
            _ => None,
        }
    }

    /// Redirect the jump at `ip` to a new absolute instruction index `target_ip`.
    /// Returns the original `Op` if successful, or `None` on error.
    ///
    /// The `target_ip` must be the byte index of the instruction to jump to.
    pub fn hotwire_redirect(&mut self, ip: usize, target_ip: usize) -> Option<Op> {
        // verify opcode + operand is present
        if ip + 1 + 1 >= self.code.len() || target_ip >= self.code.len() {
            return None;
        }
        let byte = self.code[ip];
        let op = Op::from_byte(byte)?;
        if !op.is_jump() {
            return None;
        }
        // offset = target_ip - next_instr
        let next_instr = ip + 1 + 2;
        let next_i = next_instr as i32;
        let t = target_ip as i32;
        let offset = t.checked_sub(next_i)?;
        // ensure offset fits in i16
        let offset_i16 = i16::try_from(offset).ok()?;
        let ok = self.write_i16_at(ip + 1, offset_i16);
        if !ok {
            return None;
        }
        Some(op)
    }
}

impl Default for Chunk {
    fn default() -> Self {
        Self::new()
    }
}

#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq)]
/// Bytecode operation codes
///
/// NOTE: The numeric discriminant of each `Op` variant is part of the runtime
/// bytecode format. Do not reorder or insert variants in the middle without
/// updating the bytecode version and migration logic.
pub enum Op {
    /// Push constant from constant pool
    Const = 0,
    /// Push null value
    Null,
    /// Push true value
    True,
    /// Push false value
    False,
    /// Get local variable from slot
    GetLocal,
    /// Set local variable in slot
    SetLocal,
    /// Rebind a local slot to a fresh cell containing its current value
    CloneLocalCell,
    /// Get global variable
    GetGlobal,
    /// Set global variable
    SetGlobal,
    /// Pop value from stack
    Pop,
    /// Duplicate top of stack
    Dup,
    /// Addition
    Add,
    /// Addition specialized for numeric operands (no string coercion)
    AddNumber,
    /// Subtraction
    Sub,
    /// Multiplication
    Mul,
    /// Exponentiation (**)
    Pow,
    /// Division
    Div,
    /// Modulo
    Mod,
    /// Equality comparison
    Eq,
    /// Inequality comparison
    Neq,
    /// Less than comparison
    Lt,
    /// Less than or equal comparison
    Le,
    /// Greater than comparison
    Gt,
    /// Greater than or equal comparison
    Ge,
    /// Logical and
    And,
    /// Logical or
    Or,
    /// Numeric negation
    Neg,
    /// Logical not
    Not,
    /// Typeof operator
    Typeof,
    /// Function call
    Call,
    /// Function call preserving an explicit receiver (`this`)
    CallMethod,
    /// Return from function
    Return,
    /// Unconditional jump
    Jump,
    /// Jump if top of stack is falsy
    JumpIfFalse,
    /// Build array from stack values
    BuildArray,
    /// Build object from stack values
    BuildObject,
    /// Get object property
    GetProperty,
    /// Get property with a per-op inline cache slot (operand = cache slot index)
    GetPropertyIC,
    /// Set object property
    SetProperty,
    /// Create closure from function prototype
    MakeClosure,
    /// Push the current `this` binding for the running frame
    GetThis,
    /// Read an upvalue from the closure environment (operand = upvalue index)
    GetUpvalue,
    /// Write an upvalue into the closure environment (operand = upvalue index)
    SetUpvalue,
    /// Get built-in function
    GetBuiltin,
    /// Throw an exception value (pop value and trigger handler)
    Throw,
    /// Push an exception handler record. Operand: signed 16-bit offset to handler start
    PushHandler,
    /// Pop the top exception handler record
    PopHandler,
    /// Read an export slot from a module table. Operands: module_index (u8), export_name_constant_index (u8)
    GetModuleExport,
    /// Write an export slot in a module table. Operands: module_index (u8), export_name_constant_index (u8)
    SetModuleExport,
    /// Bitwise AND
    BitAnd,
    /// Bitwise OR
    BitOr,
    /// Bitwise XOR
    BitXor,
    /// Signed left shift
    Shl,
    /// Signed right shift (arithmetic)
    Shr,
    /// Unsigned right shift (logical)
    Ushr,
    /// Await a promise (suspend execution if pending)
    AwaitPromise,
    /// Strict equality comparison (===)
    StrictEq,
    /// Strict inequality comparison (!==)
    StrictNeq,
}

impl Op {
    /// Converts a byte to an Op if valid, returns None otherwise
    pub fn from_byte(b: u8) -> Option<Op> {
        Op::try_from(b).ok()
    }

    /// Converts an Op to a byte
    pub fn to_byte(&self) -> u8 {
        u8::from(*self)
    }

    /// Returns a static slice of jump opcodes (no allocation).
    pub fn jumps() -> &'static [Op] {
        static JUMPS: [Op; 2] = [Op::Jump, Op::JumpIfFalse];
        &JUMPS
    }

    /// Returns whether this opcode is a jump (conditional or unconditional).
    pub fn is_jump(&self) -> bool {
        matches!(self, Op::Jump | Op::JumpIfFalse)
    }

    /// Returns whether this opcode performs an arithmetic operation that
    /// expects two operands.
    pub fn is_arithmetic(&self) -> bool {
        matches!(
            self,
            Op::Add | Op::AddNumber | Op::Sub | Op::Mul | Op::Pow | Op::Div | Op::Mod
        )
    }

    /// Returns whether this opcode performs a comparison operation.
    pub fn is_comparison(&self) -> bool {
        matches!(self, Op::Eq | Op::Neq | Op::Lt | Op::Le | Op::Gt | Op::Ge)
    }

    /// Returns whether this opcode is a logical or boolean operator.
    pub fn is_logical(&self) -> bool {
        matches!(self, Op::And | Op::Or | Op::Not)
    }

    /// Returns whether this opcode manipulates the value stack directly
    /// without invoking arithmetic or logical semantics.
    pub fn is_stack_operation(&self) -> bool {
        matches!(self, Op::Pop | Op::Dup)
    }
}

// Safe conversions between `u8` and `Op`.
// Implementing TryFrom<u8> avoids the previous unsafe `transmute` approach.
impl TryFrom<u8> for Op {
    type Error = ();

    fn try_from(b: u8) -> Result<Self, Self::Error> {
        match b {
            0 => Ok(Op::Const),
            1 => Ok(Op::Null),
            2 => Ok(Op::True),
            3 => Ok(Op::False),
            4 => Ok(Op::GetLocal),
            5 => Ok(Op::SetLocal),
            6 => Ok(Op::CloneLocalCell),
            7 => Ok(Op::GetGlobal),
            8 => Ok(Op::SetGlobal),
            9 => Ok(Op::Pop),
            10 => Ok(Op::Dup),
            11 => Ok(Op::Add),
            12 => Ok(Op::AddNumber),
            13 => Ok(Op::Sub),
            14 => Ok(Op::Mul),
            15 => Ok(Op::Pow),
            16 => Ok(Op::Div),
            17 => Ok(Op::Mod),
            18 => Ok(Op::Eq),
            19 => Ok(Op::Neq),
            20 => Ok(Op::Lt),
            21 => Ok(Op::Le),
            22 => Ok(Op::Gt),
            23 => Ok(Op::Ge),
            24 => Ok(Op::And),
            25 => Ok(Op::Or),
            26 => Ok(Op::Neg),
            27 => Ok(Op::Not),
            28 => Ok(Op::Typeof),
            29 => Ok(Op::Call),
            30 => Ok(Op::CallMethod),
            31 => Ok(Op::Return),
            32 => Ok(Op::Jump),
            33 => Ok(Op::JumpIfFalse),
            34 => Ok(Op::BuildArray),
            35 => Ok(Op::BuildObject),
            36 => Ok(Op::GetProperty),
            37 => Ok(Op::GetPropertyIC),
            38 => Ok(Op::SetProperty),
            39 => Ok(Op::MakeClosure),
            40 => Ok(Op::GetThis),
            41 => Ok(Op::GetUpvalue),
            42 => Ok(Op::SetUpvalue),
            43 => Ok(Op::GetBuiltin),
            44 => Ok(Op::Throw),
            45 => Ok(Op::PushHandler),
            46 => Ok(Op::PopHandler),
            47 => Ok(Op::GetModuleExport),
            48 => Ok(Op::SetModuleExport),
            49 => Ok(Op::BitAnd),
            50 => Ok(Op::BitOr),
            51 => Ok(Op::BitXor),
            52 => Ok(Op::Shl),
            53 => Ok(Op::Shr),
            54 => Ok(Op::Ushr),
            55 => Ok(Op::AwaitPromise),
            56 => Ok(Op::StrictEq),
            57 => Ok(Op::StrictNeq),
            _ => Err(()),
        }
    }
}

impl From<Op> for u8 {
    fn from(op: Op) -> Self {
        op as u8
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn op_round_trip() {
        for i in 0u8..=Op::AwaitPromise as u8 {
            let op = Op::try_from(i).expect("valid op");
            let b = u8::from(op);
            assert_eq!(b, i);
            let op2 = Op::from_byte(b).expect("from_byte");
            assert_eq!(op2, op);
        }
        assert!(Op::try_from(255u8).is_err());
    }

    #[test]
    fn hotwire_jump_helpers() {
        // Build a simple chunk with: Jump <offset to later>, followed by a small body.
        // Layout: [Jump opcode][offset lo][offset hi][...]
        let mut c = Chunk::new();
        // reserve some code so indices are meaningful
        // instruction at 0: Jump -> target will be at instruction index 6 (for example)
        c.code = vec![
            Op::Jump.to_byte(),
            0x03,
            0x00, // offset = 3 (little-endian) => target = next_instr (0+1+2=3) + 3 = 6
            // filler bytes for instructions at indices 3..6
            0xFF,
            0xFF,
            0xFF,
            0x00,
        ];
        // verify jump_target_from_ip
        let target = c.jump_target_from_ip(0).expect("target");
        // compute expected: next_instr = 3, offset = 3 -> target = 6
        assert_eq!(target, 6usize);
        // hotwire to next: sets offset = 0 => target becomes next_instr (3)
        let orig = c.hotwire_jump_to_next(0).expect("hotwire");
        assert_eq!(orig, Op::Jump);
        let t2 = c.jump_target_from_ip(0).expect("target2");
        assert_eq!(t2, 3usize);

        // make unconditional should be a no-op for Jump
        let orig2 = c.hotwire_make_unconditional(0).expect("make_uncond");
        assert_eq!(orig2, Op::Jump);

        // redirect to absolute ip 4
        let orig3 = c.hotwire_redirect(0, 4).expect("redirect");
        assert_eq!(orig3, Op::Jump);
        let t3 = c.jump_target_from_ip(0).expect("target3");
        assert_eq!(t3, 4usize);
    }
}
