//! Bytecode format: opcodes and chunk. Types are retained for runtime checks.

use crate::ast::Type;
use crate::value::Value;
use std::collections::HashMap;
use std::rc::Rc;

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
    /// Type of each global (name -> type). Used at runtime for SetGlobal checks.
    /// A missing (`None`) entry indicates no static type annotation was provided.
    pub global_types: HashMap<String, Option<Type>>,
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
    /// Type of each slot (params then locals). Used at runtime for SetLocal checks.
    /// Each slot may be `None` when no static annotation was provided.
    pub slot_types: Vec<Option<Type>>,
    /// Whether the last parameter is a rest parameter (`...args`). If true,
    /// at runtime the last local will be populated with an array of the
    /// remaining arguments.
    pub has_rest: bool,
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
            global_types: HashMap::new(),
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
}

impl Default for Chunk {
    fn default() -> Self {
        Self::new()
    }
}

#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq)]
/// Bytecode operation codes
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
    /// Get built-in function
    GetBuiltin,
    /// Throw an exception value (pop value and trigger handler)
    Throw,
    /// Push an exception handler record. Operand: signed 16-bit offset to handler start
    PushHandler,
    /// Pop the top exception handler record
    PopHandler,
}

impl Op {
    /// Converts a byte to an Op if valid, returns None otherwise
    pub fn from_byte(b: u8) -> Option<Op> {
        // Accept any byte that is a valid discriminant for the Op enum.
        // Compare against the maximum defined opcode value (PopHandler).
        if b <= Op::PopHandler as u8 {
            // SAFETY: `b` is within the enum range, transmute is safe.
            Some(unsafe { std::mem::transmute::<u8, Op>(b) })
        } else {
            None
        }
    }
}
