// SPDX-License-Identifier: MPL-2.0
//! Bytecode representation and VM instruction set.
//!
//! This module defines the bytecode format used by the VM, including:
//! - [`Opcode`] enum: All VM opcodes with safe conversions to/from `u8`
//! - [`Chunk`] struct: Instruction words, constants, and nested functions
//! - [`Function`] struct: A compiled ECMAScript function and its metadata
//!
//! # Instruction Format
//!
//! All instructions are single 32-bit words with this layout:
//! - Bits 0-7:    Opcode (u8)
//! - Bits 8-11:   Destination register rd (4 bits, r0-r15)
//! - Bits 12-15:  Source register rs (4 bits, r0-r15)
//! - Bits 16-23:  Auxiliary data (u8) - interpretation depends on opcode
//! - Bits 24-31:  Auxiliary data (u8) - interpretation depends on opcode
//!
//! Special cases:
//! - Jump instructions: bits 8-31 contain i24 signed offset
//! - bytecode CompareJump: cond(3) | rs1(4) | rs2(4) | signed offset(13)
//!
//! # Example
//!
//! ```
//! use denko::bytecode::{Chunk, Opcode};
//! use std::sync::Arc;
//!
//! let mut code = Chunk::new(Arc::new(String::new()));
//! code.write_instr(Opcode::Const as u32, 1); // constant index 0
//! ```

use crate::typecheck::Type;
use crate::value::{SymbolId, Value};
use js_string::JsString;
use num_bigint::BigInt;
use oxc_span::Span;
use std::convert::TryFrom;
use std::sync::Arc;

pub type InstructionWord = u32;
pub type InstructionIndex = usize;

/// Description of a captured binding for a closure.
#[derive(Clone, Debug)]
pub struct UpvalueSpec {
  /// True when capturing from the immediately enclosing function's locals;
  /// false when capturing from the enclosing function's own upvalue list.
  pub is_local: bool,
  /// Slot or upvalue index in the enclosing function.
  pub index: u16,
  /// Optional global binding name that should mirror writes to this captured
  /// cell. Used for root `var` bindings that are also exposed on the global
  /// object.
  pub mirror_global_name: Option<String>,
  /// Whether this captured binding is immutable (`const`/function-name binding).
  pub immutable: bool,
  /// Whether writes in non-strict mode should be silently ignored.
  pub sloppy_immutable_silent: bool,
}

/// Version marker carried by every chunk.
///
/// Blitz has a single executable bytecode contract. The explicit marker is
/// retained so serialized code can be rejected cleanly if the contract is
/// changed in the future.
#[repr(u8)]
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum BytecodeVersion {
  /// Fixed-width encoding interpreted with the 16-register bytecode v2 semantics.
  #[default]
  RiscV2 = 2,
}

#[repr(u8)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Register {
  R0,
  R1,
  R2,
  R3,
  R4,
  R5,
  R6,
  R7,
  R8,
  R9,
  R10,
  R11,
  R12,
  R13,
  R14,
  R15,
}

impl Register {
  pub const COUNT: usize = 16;

  pub const fn index(self) -> usize {
    self as usize
  }

  pub const fn encoding(self) -> u8 {
    self as u8
  }
}

impl TryFrom<u8> for Register {
  type Error = u8;

  fn try_from(value: u8) -> Result<Self, Self::Error> {
    match value {
      0 => Ok(Self::R0),
      1 => Ok(Self::R1),
      2 => Ok(Self::R2),
      3 => Ok(Self::R3),
      4 => Ok(Self::R4),
      5 => Ok(Self::R5),
      6 => Ok(Self::R6),
      7 => Ok(Self::R7),
      8 => Ok(Self::R8),
      9 => Ok(Self::R9),
      10 => Ok(Self::R10),
      11 => Ok(Self::R11),
      12 => Ok(Self::R12),
      13 => Ok(Self::R13),
      14 => Ok(Self::R14),
      15 => Ok(Self::R15),
      _ => Err(value),
    }
  }
}

pub mod abi {
  use super::Register;

  pub const RETURN: Register = Register::R0;
  pub const V2_CALL_OPERAND: Register = Register::R0;
  pub const RECEIVER: Register = Register::R1;
  pub const FIRST_ARGUMENT: Register = Register::R2;
  pub const LAST_ARGUMENT: Register = Register::R7;
  pub const EXCEPTION: Register = Register::R7;
  pub const FIRST_TEMPORARY: Register = Register::R8;
  pub const LAST_TEMPORARY: Register = Register::R15;
  pub const REGISTER_ARGUMENT_COUNT: usize = 6;

  pub fn argument_register(index: usize) -> Option<Register> {
    let encoding = FIRST_ARGUMENT
      .encoding()
      .checked_add(u8::try_from(index).ok()?)?;
    (encoding <= LAST_ARGUMENT.encoding()).then(|| {
      Register::try_from(encoding).expect("ABI argument register must be in the register file")
    })
  }

  pub const fn is_argument_register(register: Register) -> bool {
    register.encoding() >= FIRST_ARGUMENT.encoding()
      && register.encoding() <= LAST_ARGUMENT.encoding()
  }

  pub const fn is_temporary_register(register: Register) -> bool {
    register.encoding() >= FIRST_TEMPORARY.encoding()
      && register.encoding() <= LAST_TEMPORARY.encoding()
  }
}

/// A decoded fixed-width instruction.
#[derive(Clone, Copy, Debug)]
pub struct Instruction {
  /// Decoded opcode.
  pub opcode: Opcode,
  /// Unsigned 24-bit payload.
  pub payload: u32,
}

impl Instruction {
  pub fn decode(word: InstructionWord) -> Result<Self, String> {
    let opcode = opcode_from_word(word);
    let op = Opcode::from_byte(opcode).ok_or_else(|| format!("invalid opcode 0x{opcode:02x}"))?;
    Ok(Self {
      opcode: op,
      payload: word >> 8,
    })
  }

  pub(crate) unsafe fn decode_unchecked(word: InstructionWord) -> Self {
    Self {
      opcode: unsafe { Opcode::from_byte_unchecked(opcode_from_word(word)) },
      payload: word >> 8,
    }
  }

  /// Return one of the three packed payload bytes.
  pub fn operand_u8(
    self,
    index: usize,
  ) -> Option<u8> {
    (index < 3).then(|| ((self.payload >> (index * 8)) & 0xFF) as u8)
  }

  /// Return the low 16 payload bits as an unsigned operand.
  pub fn operand_u16(self) -> u16 {
    (self.payload & 0xFFFF) as u16
  }

  /// Return the sign-extended 24-bit payload.
  pub fn operand_i24(self) -> i32 {
    let payload = self.payload & 0x00FF_FFFF;
    if payload & 0x0080_0000 != 0 {
      (payload | 0xFF00_0000) as i32
    } else {
      payload as i32
    }
  }

  /// Relative branch offset, accounting for opcode-specific payload layouts.
  pub fn branch_offset(self) -> Option<i32> {
    match self.opcode {
      Opcode::CompareJump => {
        let word = self.opcode as u32 | ((self.payload & 0x00FF_FFFF) << 8);
        Some(decode_compare_jump(word).3 as i32)
      }
      Opcode::JumpIfFalse | Opcode::JumpIfTrue | Opcode::JumpIfNullish => {
        let word = self.opcode as u32 | ((self.payload & 0x00FF_FFFF) << 8);
        Some(decode_conditional_branch_offset(word))
      }
      Opcode::Jump | Opcode::PushHandler => Some(self.operand_i24()),
      _ => None,
    }
  }
}

/// An immutable literal stored in compiled bytecode.
///
/// Runtime-owned objects are deliberately excluded so compiled code can be
/// transferred from compiler workers to the single VM thread.
#[derive(Clone, Debug, PartialEq)]
pub enum Constant {
  /// Internal sentinel for temporal dead zone bindings.
  Uninitialized,
  /// Number literal.
  Number(f64),
  /// String literal.
  String(JsString),
  /// Boolean literal.
  Bool(bool),
  /// Undefined literal.
  Undefined,
  /// BigInt literal.
  BigInt(BigInt),
  /// Symbol literal.
  Symbol(SymbolId, Option<JsString>),
  /// Null literal.
  Null,
}

impl Constant {
  /// Materialize this compiler-owned literal as a runtime value.
  pub fn to_value(&self) -> Value {
    match self {
      Self::Uninitialized => Value::Uninitialized,
      Self::Number(value) => Value::Number(*value),
      Self::String(value) => Value::String(value.clone()),
      Self::Bool(value) => Value::Bool(*value),
      Self::Undefined => Value::Undefined,
      Self::BigInt(value) => Value::BigInt(value.clone()),
      Self::Symbol(id, description) => Value::Symbol(id.clone(), description.clone()),
      Self::Null => Value::Null,
    }
  }
}

/// Error returned when a runtime-owned value is passed to the bytecode
/// constant pool.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ConstantConversionError;

impl TryFrom<Value> for Constant {
  type Error = ConstantConversionError;

  fn try_from(value: Value) -> Result<Self, Self::Error> {
    match value {
      Value::Uninitialized => Ok(Self::Uninitialized),
      Value::Number(value) => Ok(Self::Number(value)),
      Value::String(value) => Ok(Self::String(value)),
      Value::Bool(value) => Ok(Self::Bool(value)),
      Value::Undefined => Ok(Self::Undefined),
      Value::BigInt(value) => Ok(Self::BigInt(value)),
      Value::Symbol(id, description) => Ok(Self::Symbol(id, description)),
      Value::Null => Ok(Self::Null),
      _ => Err(ConstantConversionError),
    }
  }
}

/// Executable instruction words and their shared metadata.
#[derive(Clone, Debug)]
pub struct Chunk {
  /// Version of the encoded instruction words.
  pub version: BytecodeVersion,
  /// Instructions stored as fixed-width 32-bit words. Each u32 contains
  /// opcode in the low 8 bits and a 24-bit payload in the upper bits.
  pub words: Vec<InstructionWord>,
  /// Constant pool values
  pub constants: Vec<Constant>,
  /// Nested compiled functions.
  pub functions: Vec<Function>,
  /// Line number information for debugging
  pub lines: Vec<u32>,
  /// Original source text for this code
  pub source_text: Arc<String>,
  /// Root script slot type annotations.
  pub root_slot_types: Vec<Option<Type>>,
  /// Root script binding names.
  pub root_slot_names: Vec<Option<String>>,
  /// Root script temporal dead zone flags.
  pub root_lexical_tdz: Vec<bool>,
  /// Root script immutability flags (`const`).
  pub root_slot_immutable: Vec<bool>,
  /// Function declarations instantiated in the root variable environment.
  pub root_function_declarations: Vec<String>,
  /// Number of non-lexical frame spill slots required by bytecode v2 lowering.
  pub spill_slot_count: u16,
  /// Prefix of `spill_slot_count` occupied by stable lowered value homes.
  pub value_spill_slot_count: u16,
  /// Whether the compiled root source is strict.
  pub root_strict: bool,
  /// Whether the root module syntax contains await, including unreachable code.
  pub has_top_level_await: bool,
  /// Whether `new.target` is valid in this code object's execution context.
  pub allow_new_target: bool,
  /// Whether `super` property access/calls are valid in this code object.
  pub allow_super: bool,
  /// Finalizer entry points used to inject generator `return` completions.
  pub finally_handlers: Vec<FinallyHandler>,
}

/// Bytecode that has passed structural verification and can be executed
/// repeatedly without rescanning the instruction stream.
#[derive(Clone, Debug)]
pub struct VerifiedChunk(Arc<Chunk>);

impl VerifiedChunk {
  /// Verify and retain chunk for repeated execution.
  pub fn new(chunk: Chunk) -> Result<Self, String> {
    chunk.verify()?;
    Ok(Self(Arc::new(chunk)))
  }

  pub(crate) fn chunk(&self) -> Arc<Chunk> {
    Arc::clone(&self.0)
  }
}

#[derive(Clone, Debug)]
/// Control-flow metadata for a protected region with a `finally` clause.
pub struct FinallyHandler {
  /// Instruction that installs the protected region's exception handler.
  pub push_pc: usize,
  /// First instruction in the normal finalizer body.
  pub finalizer_pc: usize,
  /// Local slot that carries an injected generator return value.
  pub return_slot: usize,
  /// Local slot indicating that the finalizer must complete with a return.
  pub did_return_slot: usize,
}

#[derive(Clone, Debug)]
/// Function prototype containing metadata for compiled functions
pub struct Function {
  /// Function name
  pub name: String,
  /// Source span in the original script
  pub span: Span,
  /// Number of parameter slots used to initialize a call frame.
  pub arity: usize,
  /// ECMAScript `length` value: parameters before the first default, excluding rest.
  pub length: usize,
  /// Compiled bytecode for this function
  pub chunk: Arc<Chunk>,
  /// Optional static slot types retained for tooling and serialization; the
  /// runtime does not enforce TypeScript annotations.
  pub slot_types: Vec<Option<Type>>,
  /// Per-slot binding names when available.
  pub slot_names: Vec<Option<String>>,
  /// Per-slot temporal dead zone flags for lexical bindings (`let`/`const`).
  /// When true, the slot must start as `Value::Uninitialized` and throw on
  /// read until initialized.
  pub lexical_tdz: Vec<bool>,
  /// Per-slot immutability flags (`const` and named function-expression self bindings).
  pub slot_immutable: Vec<bool>,
  /// Per-slot flag for immutable bindings that ignore sloppy writes.
  pub slot_sloppy_immutable_silent: Vec<bool>,
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
  /// Slot index for a named function-expression self binding, if reserved.
  pub self_binding_slot: Option<usize>,
  /// Whether the formal parameter list is simple enough for mapped arguments.
  pub has_simple_parameter_list: bool,
  /// Return type annotation
  pub return_type: Option<Type>,
  /// Whether this function contains a "use strict" directive in its
  /// directive prologue. Used at runtime to set the per-function strict
  /// mode flag on the call frame.
  pub is_strict: bool,
  /// Whether this function may be used as a constructor (per ECMAScript `Construct`).
  pub is_constructor: bool,
  /// Whether this constructor is a derived class constructor.
  pub is_derived_constructor: bool,
  /// Whether this function was created from a `class` constructor definition.
  /// Such functions are only constructable with `new` and must throw on direct call.
  pub is_class_constructor: bool,
  /// Whether this function was declared as `async`.
  pub is_async: bool,
  /// Whether this function was declared as a generator (`function*`).
  pub is_generator: bool,
}

impl Chunk {
  /// Creates an empty chunk.
  pub fn new(source_text: Arc<String>) -> Self {
    Self {
      version: BytecodeVersion::RiscV2,
      words: Vec::new(),
      constants: Vec::new(),
      functions: Vec::new(),
      lines: Vec::new(),
      source_text,
      root_slot_types: Vec::new(),
      root_slot_names: Vec::new(),
      root_lexical_tdz: Vec::new(),
      root_slot_immutable: Vec::new(),
      root_function_declarations: Vec::new(),
      spill_slot_count: 0,
      value_spill_slot_count: 0,
      root_strict: false,
      has_top_level_await: false,
      allow_new_target: false,
      allow_super: false,
      finally_handlers: Vec::new(),
    }
  }

  /// Decode one bytecode v2 instruction word.
  pub fn decode_at(
    &self,
    pc: InstructionIndex,
  ) -> Result<Instruction, String> {
    let word = self
      .words
      .get(pc)
      .copied()
      .ok_or_else(|| format!("instruction index {} is out of bounds", pc))?;
    Instruction::decode(word).map_err(|error| format!("{error} at instruction {pc}"))
  }

  /// Format one instruction for tracing and diagnostics.
  pub fn disassemble_at(
    &self,
    pc: usize,
  ) -> Result<String, String> {
    let instruction = self.decode_at(pc)?;
    let word = self.words[pc];
    let detail = match instruction.opcode {
      Opcode::CompareJump => {
        let (condition, left, right, offset) = decode_compare_jump(word);
        format!(
          "compare={} r{} r{} offset={}",
          condition, left, right, offset
        )
      }
      Opcode::JumpIfFalse | Opcode::JumpIfTrue | Opcode::JumpIfNullish => {
        format!(
          "r{} offset={}",
          decode_rd(word),
          decode_conditional_branch_offset(word)
        )
      }
      Opcode::Jump | Opcode::PushHandler => {
        format!("offset={}", instruction.operand_i24())
      }
      _ => format!(
        "r{} r{} aux={} aux2={}",
        decode_rd(word),
        decode_rs(word),
        decode_aux8(word),
        decode_aux8_2(word)
      ),
    };
    if detail.is_empty() {
      Ok(format!("{:#06} {}", pc, instruction.opcode.name()))
    } else {
      Ok(format!(
        "{:#06} {} {}",
        pc,
        instruction.opcode.name(),
        detail
      ))
    }
  }

  /// Validate structural invariants before a code is executed.
  pub fn verify(&self) -> Result<(), String> {
    let root_slot_count = self.root_slot_types.len();
    for (name, length) in [
      ("root slot names", self.root_slot_names.len()),
      ("root TDZ flags", self.root_lexical_tdz.len()),
      ("root immutability flags", self.root_slot_immutable.len()),
    ] {
      if length != root_slot_count {
        return Err(format!(
          "{name} length {length} does not match root slot count {root_slot_count}"
        ));
      }
    }
    self.verify_with_layout(root_slot_count, 0)
  }

  fn verify_with_layout(
    &self,
    local_count: usize,
    upvalue_count: usize,
  ) -> Result<(), String> {
    if self.lines.len() != self.words.len() {
      return Err(format!(
        "line table length {} does not match instruction count {}",
        self.lines.len(),
        self.words.len()
      ));
    }
    if self.value_spill_slot_count > self.spill_slot_count {
      return Err(format!(
        "value spill count {} exceeds total spill count {}",
        self.value_spill_slot_count, self.spill_slot_count
      ));
    }

    for pc in 0..self.words.len() {
      let instruction = self.decode_at(pc)?;
      let word = self.words[pc];
      if let Some(offset) = instruction.branch_offset() {
        let target = pc as i64 + 1 + offset as i64;
        if !(0..self.words.len() as i64).contains(&target) {
          return Err(format!(
            "jump at instruction {} targets out-of-bounds instruction {}",
            pc, target
          ));
        }
      }

      let operand = decode_aux16(word) as usize;
      match instruction.opcode {
        Opcode::Const => self.verify_constant_index(pc, operand, false)?,
        Opcode::GetGlobal | Opcode::TypeofGlobal | Opcode::SetGlobal | Opcode::GetBuiltin => {
          self.verify_constant_index(pc, operand, true)?
        }
        Opcode::GetModuleExport | Opcode::SetModuleExport => {
          self.verify_module_descriptor(pc, operand)?;
        }
        Opcode::MakeClosure if operand >= self.functions.len() => {
          return Err(format!(
            "function index {operand} at instruction {pc} is out of bounds"
          ));
        }
        Opcode::LoadLocal
        | Opcode::StoreLocal
        | Opcode::CloneLocalCell
        | Opcode::ResetLocalCell
          if operand >= local_count =>
        {
          return Err(format!(
            "local slot {operand} at instruction {pc} is out of bounds"
          ));
        }
        Opcode::GetUpvalue | Opcode::SetUpvalue if operand >= upvalue_count => {
          return Err(format!(
            "upvalue slot {operand} at instruction {pc} is out of bounds"
          ));
        }
        Opcode::LoadSpill | Opcode::StoreSpill if operand >= self.spill_slot_count as usize => {
          return Err(format!(
            "spill slot {operand} at instruction {pc} is out of bounds"
          ));
        }
        Opcode::Call | Opcode::TailCall | Opcode::Construct | Opcode::CallMethod => {
          if decode_rd(word) != abi::V2_CALL_OPERAND.encoding()
            || decode_rs(word) != abi::V2_CALL_OPERAND.encoding()
          {
            return Err(format!("call ABI registers at instruction {pc} must be r0"));
          }
          self.verify_operand_window(pc, operand.saturating_sub(abi::REGISTER_ARGUMENT_COUNT))?;
        }
        Opcode::BuildArray | Opcode::SuperConstruct => {
          self.verify_operand_window(pc, operand)?;
        }
        Opcode::BuildObject => {
          let required = operand
            .checked_mul(2)
            .ok_or_else(|| format!("object operand window at instruction {pc} overflows"))?;
          self.verify_operand_window(pc, required)?;
        }
        Opcode::SetProperty | Opcode::SetSuperProperty | Opcode::Spread
          if decode_aux8(word) > 15 || decode_aux8_2(word) != 0 =>
        {
          return Err(format!(
            "extra register operand at instruction {pc} is malformed"
          ));
        }
        _ => {}
      }
    }

    for (index, handler) in self.finally_handlers.iter().enumerate() {
      if self.finally_handlers[..index]
        .iter()
        .any(|other| other.push_pc == handler.push_pc)
      {
        return Err(format!(
          "finally handler push instruction {} is duplicated",
          handler.push_pc
        ));
      }
      if self
        .words
        .get(handler.push_pc)
        .and_then(|word| Opcode::from_byte(opcode_from_word(*word)))
        != Some(Opcode::PushHandler)
      {
        return Err(format!(
          "finally handler push instruction {} is invalid",
          handler.push_pc
        ));
      }
      if handler.finalizer_pc >= self.words.len() {
        return Err(format!(
          "finally handler target {} is out of bounds",
          handler.finalizer_pc
        ));
      }
      if handler.return_slot >= local_count || handler.did_return_slot >= local_count {
        return Err("finally handler local slot is out of bounds".to_string());
      }
    }

    for function in &self.functions {
      function.verify_metadata(local_count, upvalue_count)?;
    }
    Ok(())
  }

  fn verify_constant_index(
    &self,
    pc: usize,
    index: usize,
    require_string: bool,
  ) -> Result<(), String> {
    let constant = self
      .constants
      .get(index)
      .ok_or_else(|| format!("constant index {index} at instruction {pc} is out of bounds"))?;
    if require_string && !matches!(constant, Constant::String(_)) {
      return Err(format!(
        "constant index {index} at instruction {pc} must contain a string"
      ));
    }
    Ok(())
  }

  fn verify_module_descriptor(
    &self,
    pc: usize,
    index: usize,
  ) -> Result<(), String> {
    self.verify_constant_index(pc, index, true)?;
    let Constant::String(descriptor) = &self.constants[index] else {
      unreachable!("string constant verified above");
    };
    let descriptor = descriptor.to_string_lossy();
    let Some((module, _name)) = descriptor.split_once('\0') else {
      return Err(format!(
        "module descriptor at constant index {index} is malformed"
      ));
    };
    if module.parse::<usize>().is_err() {
      return Err(format!(
        "module descriptor at constant index {index} is malformed"
      ));
    }
    Ok(())
  }

  fn verify_operand_window(
    &self,
    pc: usize,
    required: usize,
  ) -> Result<(), String> {
    let available = usize::from(self.spill_slot_count - self.value_spill_slot_count);
    if required > available {
      return Err(format!(
        "operand window of {required} values at instruction {pc} exceeds {available} slots"
      ));
    }
    Ok(())
  }

  /// Writes a packed instruction word to the code with line number info.
  /// The instruction layout: low 8 bits = opcode, upper 24 bits = payload.
  pub fn write_instr(
    &mut self,
    instr: u32,
    line: u32,
  ) {
    self.words.push(instr);
    self.lines.push(line);
  }

  /// Write a single opcode as an instruction word with an empty payload.
  pub fn write(
    &mut self,
    opcode: u8,
    line: u32,
  ) {
    let instr = opcode as u32;
    self.write_instr(instr, line);
  }

  /// Adds a constant to the constant pool and returns its index
  pub fn add_constant(
    &mut self,
    value: Constant,
  ) -> usize {
    self.constants.push(value);
    self.constants.len() - 1
  }

  /// Adds a function prototype to the function list and returns its index
  pub fn add_function(
    &mut self,
    proto: Function,
  ) -> usize {
    self.functions.push(proto);
    self.functions.len() - 1
  }

  /// Read the 24-bit signed payload (bits 8..31) from instruction at `idx`.
  /// Returns None if out of bounds.
  pub fn read_i24_payload_at(
    &self,
    idx: usize,
  ) -> Option<i32> {
    let w = *self.words.get(idx)?;
    let payload = (w >> 8_i32) & 0x00FF_FFFF;
    let signed = if (payload & 0x0080_0000) != 0 {
      (payload | 0xFF00_0000) as i32
    } else {
      payload as i32
    };
    Some(signed)
  }

  /// Write a 24-bit signed payload into instruction at `idx`.
  /// Returns false if out of bounds or payload doesn't fit in 24 bits.
  pub fn write_i24_payload_at(
    &mut self,
    idx: usize,
    val: i32,
  ) -> bool {
    if idx >= self.words.len() {
      return false;
    }
    if !(-(1_i32 << 23_i32)..(1_i32 << 23_i32)).contains(&val) {
      return false;
    }
    let instr = self.words[idx];
    let payload_u32 = (val.cast_unsigned() & 0x00FF_FFFF) << 8_u32;
    let new = (instr & 0x0000_00FF) | payload_u32;
    self.words[idx] = new;
    true
  }

  /// Write a signed 16-bit payload into the instruction at `idx` by
  /// sign-extending it into the instruction's 24-bit payload field.
  pub fn write_i16_at(
    &mut self,
    idx: usize,
    val: i16,
  ) -> bool {
    self.write_i24_payload_at(idx, val as i32)
  }

  /// Return the instruction-index target of a branch at `pc`.
  pub fn jump_target_from_pc(
    &self,
    pc: usize,
  ) -> Option<usize> {
    let instr = *self.words.get(pc)?;
    let op_byte = opcode_from_word(instr);
    let op = Opcode::from_byte(op_byte)?;
    if !op.is_jump() {
      return None;
    }
    let offset = self.decode_at(pc).ok()?.branch_offset()?;
    let next = pc as i64 + 1;
    let target = next.checked_add(offset as i64)?;
    if target < 0 || target >= (self.words.len() as i64) {
      return None;
    }
    usize::try_from(target).ok()
  }
}

/// Encodes a destination register into bits 8-11 of an instruction
#[inline]
pub fn encode_rd(rd: u8) -> u32 {
  ((rd as u32) & 0x0F) << 8
}

/// Decodes the destination register from bits 8-11 of an instruction
#[inline]
pub fn decode_rd(instr: u32) -> u8 {
  ((instr >> 8) & 0x0F) as u8
}

/// Encodes a source register into bits 12-15 of an instruction
#[inline]
pub fn encode_rs(rs: u8) -> u32 {
  ((rs as u32) & 0x0F) << 12
}

/// Decodes the source register from bits 12-15 of an instruction
#[inline]
pub fn decode_rs(instr: u32) -> u8 {
  ((instr >> 12) & 0x0F) as u8
}

/// Encodes auxiliary data (u8) into bits 16-23 of an instruction
#[inline]
pub fn encode_aux8(aux: u8) -> u32 {
  (aux as u32) << 16
}

/// Decodes auxiliary data (u8) from bits 16-23 of an instruction
#[inline]
pub fn decode_aux8(instr: u32) -> u8 {
  ((instr >> 16) & 0xFF) as u8
}

/// Encodes auxiliary data (u8) into bits 24-31 of an instruction
#[inline]
pub fn encode_aux8_2(aux: u8) -> u32 {
  (aux as u32) << 24
}

/// Decodes auxiliary data (u8) from bits 24-31 of an instruction
#[inline]
pub fn decode_aux8_2(instr: u32) -> u8 {
  ((instr >> 24) & 0xFF) as u8
}

/// Encodes both rd and rs into a single u32
#[inline]
pub fn encode_register_fields(
  rd: u8,
  rs: u8,
) -> u32 {
  encode_rd(rd) | encode_rs(rs)
}

/// Encode a normal instruction with destination and source registers.
pub fn encode_register_instruction(
  op: Opcode,
  rd: u8,
  rs: u8,
) -> u32 {
  op as u32 | encode_register_fields(rd, rs)
}

/// Encode a instruction with `rd` and a 16-bit auxiliary operand.
pub fn encode_aux16_instruction(
  op: Opcode,
  rd: u8,
  aux: u16,
) -> u32 {
  op as u32 | encode_rd(rd) | ((aux as u32) << 16)
}

/// Decode the 16-bit auxiliary operand from a normal instruction.
pub fn decode_aux16(instr: u32) -> u16 {
  (instr >> 16) as u16
}

/// Encode a conditional jump using `rd` and a signed 20-bit offset.
pub fn encode_conditional_branch(
  op: Opcode,
  rd: u8,
  offset: i32,
) -> Option<u32> {
  if !matches!(
    op,
    Opcode::JumpIfFalse | Opcode::JumpIfTrue | Opcode::JumpIfNullish
  ) || !(-(1 << 19)..(1 << 19)).contains(&offset)
  {
    return None;
  }
  Some(op as u32 | encode_rd(rd) | (((offset as u32) & 0x000F_FFFF) << 12))
}

/// Decode the signed 20-bit offset from a conditional jump.
pub fn decode_conditional_branch_offset(instr: u32) -> i32 {
  let value = (instr >> 12) & 0x000F_FFFF;
  if value & 0x0008_0000 != 0 {
    (value | 0xFFF0_0000) as i32
  } else {
    value as i32
  }
}

/// Pack a compare-and-branch instruction.
///
/// The signed instruction-relative offset is limited to 13 bits. Larger
/// branches must use an inverted short branch followed by a long jump.
pub fn encode_compare_jump(
  condition: u8,
  left: u8,
  right: u8,
  offset: i16,
) -> Option<u32> {
  if condition > 7 || left > 15 || right > 15 || !(-4096..=4095).contains(&offset) {
    return None;
  }
  let payload = (condition as u32)
    | ((left as u32) << 3)
    | ((right as u32) << 7)
    | (((offset as u16 as u32) & 0x1FFF) << 11);
  Some(Opcode::CompareJump as u32 | (payload << 8))
}

/// Decode a compare-and-branch word as `(condition, left, right, offset)`.
pub fn decode_compare_jump(instr: u32) -> (u8, u8, u8, i16) {
  let payload = instr >> 8;
  let condition = (payload & 0x7) as u8;
  let left = ((payload >> 3) & 0xF) as u8;
  let right = ((payload >> 7) & 0xF) as u8;
  let raw_offset = ((payload >> 11) & 0x1FFF) as i16;
  let offset = if raw_offset & 0x1000 != 0 {
    raw_offset | !0x1FFF
  } else {
    raw_offset
  };
  (condition, left, right, offset)
}

/// Returns the opcode from `instr`.
#[inline]
pub fn opcode_from_word(instr: u32) -> u8 {
  (instr & 0xFF) as u8
}

/// Returns the `idx`th payload byte from `instr`, where `idx` is 0-based.
#[inline]
pub fn payload_byte(
  instr: u32,
  idx: usize,
) -> u8 {
  if idx > 2 {
    0
  } else {
    ((instr >> (8 * (idx + 1))) & 0xFF) as u8
  }
}

impl Function {
  fn verify_metadata(
    &self,
    enclosing_local_count: usize,
    enclosing_upvalue_count: usize,
  ) -> Result<(), String> {
    let slot_count = self.slot_types.len();
    for (name, length) in [
      ("slot names", self.slot_names.len()),
      ("TDZ flags", self.lexical_tdz.len()),
      ("immutability flags", self.slot_immutable.len()),
      (
        "sloppy immutability flags",
        self.slot_sloppy_immutable_silent.len(),
      ),
    ] {
      if length != slot_count {
        return Err(format!(
          "function {} {name} length {length} does not match slot count {slot_count}",
          self.name
        ));
      }
    }
    let local_count = slot_count.max(self.arity);
    for (name, slot) in [
      ("arguments", self.arguments_slot),
      ("self binding", self.self_binding_slot),
    ] {
      if slot.is_some_and(|slot| slot >= local_count) {
        return Err(format!(
          "function {} {name} slot is out of bounds",
          self.name
        ));
      }
    }
    for (index, (&sloppy_silent, &immutable)) in self
      .slot_sloppy_immutable_silent
      .iter()
      .zip(&self.slot_immutable)
      .enumerate()
    {
      if sloppy_silent && !immutable {
        return Err(format!(
          "function {} slot {index} silently ignores writes but is mutable",
          self.name
        ));
      }
    }
    for (index, upvalue) in self.upvalues.iter().enumerate() {
      let limit = if upvalue.is_local {
        enclosing_local_count
      } else {
        enclosing_upvalue_count
      };
      if upvalue.index as usize >= limit {
        return Err(format!(
          "function {} upvalue {index} references an out-of-bounds enclosing binding",
          self.name
        ));
      }
    }
    self
      .chunk
      .verify_with_layout(local_count, self.upvalues.len())
  }

  /// Return the exact source substring used to define this function, if tracked.
  pub fn source_text(&self) -> Option<&str> {
    let text = &self.chunk.source_text;
    let start = self.span.start as usize;
    let end = self.span.end as usize;
    if start <= end && end <= text.len() {
      Some(&text[start..end])
    } else {
      None
    }
  }
}

impl Default for Chunk {
  fn default() -> Self {
    Self::new(Arc::new(String::new()))
  }
}

#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq)]
/// Opcodes for the bytecode v2 format.
///
/// All opcodes fit in a single 32-bit word with this layout:
/// - Bits 0-7:    Opcode
/// - Bits 8-11:   rd register (destination)
/// - Bits 12-15:  rs register (source)
/// - Bits 16-31:  Auxiliary data (interpretation depends on opcode)
///
/// Special cases:
/// - Jump: aux field contains i24 signed offset (bits 8-31)
/// - CompareJump: cond(3) | rs1(4) | rs2(4) | signed offset(13)
///
/// NOTE: The numeric discriminant of each `Opcode` variant is part of the runtime
/// bytecode format. Do not reorder or insert variants in the middle without
/// defining a new bytecode version and corresponding compatibility handling.
pub enum Opcode {
  /// Load constant from pool → rd. Aux: constant index (u16)
  Const = 0,
  /// Load null → rd
  Null,
  /// Load true → rd
  True,
  /// Load false → rd
  False,
  /// Load undefined → rd
  Undefined,

  /// Load local slot → rd. Aux: slot index (u16)
  LoadLocal,
  /// Store rd → local slot. Aux: slot index (u16)
  StoreLocal,
  /// Move rs → rd (copy register)
  Move,

  /// rd = rd + rs (add or string concat)
  Add,
  /// rd = rd - rs
  Sub,
  /// rd = rd * rs
  Mul,
  /// rd = rd / rs
  Div,
  /// rd = rd % rs (modulo)
  Mod,
  /// rd = rd ** rs (exponentiation)
  Pow,
  /// rd = -rs (negation)
  Neg,

  /// rd = rd & rs (bitwise AND)
  BitAnd,
  /// rd = rd | rs (bitwise OR)
  BitOr,
  /// rd = rd ^ rs (bitwise XOR)
  BitXor,
  /// rd = rd << rs (left shift)
  Shl,
  /// rd = rd >> rs (arithmetic right shift)
  Shr,
  /// rd = rd >>> rs (unsigned right shift)
  Ushr,

  /// CompareJump: cond_type(3) | rs1(4) | rs2(4) | signed offset(13)
  /// Compares rs1 and rs2 according to cond_type, jumps if false.
  /// cond_type: 0=Eq, 1=Neq, 2=Lt, 3=Lte, 4=Gt, 5=Gte, 6=StrictEq, 7=StrictNeq
  CompareJump,
  /// Jump offset_i24 (unconditional)
  Jump,
  /// Jump offset_i20 if rd is truthy
  JumpIfTrue,
  /// Jump offset_i20 if rd is falsy
  JumpIfFalse,
  /// Jump offset_i20 if rd is null or undefined
  JumpIfNullish,

  /// Combine operand truthiness: rd = truthy(rd) && truthy(rs).
  And,
  /// Combine operand truthiness: rd = truthy(rd) || truthy(rs).
  Or,
  /// rd = !rs (logical NOT)
  Not,
  /// rd = typeof rs
  Typeof,

  /// rd = rd[rs]. The property-get cache is keyed by instruction position.
  GetProperty,
  /// rd[rs] = aux register. The value register is stored in the first aux byte.
  SetProperty,
  /// rd = delete obj[key]
  DeleteProperty,
  /// rd = (key in obj)
  HasProperty,

  /// rd = GetGlobal(name_const_idx). Aux: name constant index (u16)
  GetGlobal,
  /// rd = typeof name_const_idx. Aux: name constant index (u16)
  TypeofGlobal,
  /// SetGlobal(name_const_idx, rd). Aux: name constant index (u16)
  SetGlobal,

  /// rd = GetUpvalue(upval_idx). Aux: upvalue index (u16)
  GetUpvalue,
  /// SetUpvalue(upval_idx, rd). Aux: upvalue index (u16)
  SetUpvalue,

  /// Call the callee in r0 with arguments in r2-r7 and then the spill window.
  /// The result is written to r0. Aux: argument count (u16).
  Call,
  /// Tail-call form using the same operand convention as `Call`.
  TailCall,
  /// Construct: new r0(...). Aux: argument count (u16)
  Construct,
  /// CallMethod: call the builtin in r0 with receiver r1 and Call's arguments.
  CallMethod,

  /// rd = MakeClosure(proto_idx). Aux: prototype index (u16)
  MakeClosure,
  /// rd = GetThis
  GetThis,
  /// rd = GetNewTarget
  GetNewTarget,
  /// rd = super[rs], using the receiver/base value in rd.
  GetSuperProperty,

  /// BuildArray: collect values from the spill window into rd. Aux: element count (u16)
  BuildArray,
  /// BuildObject: collect key/value pairs from the spill window into rd. Aux: pair count (u16)
  BuildObject,
  /// ArrayFromSpread: build array with spread
  ArrayFromSpread,

  /// Throw rd
  Throw = 54,
  /// PushHandler: push exception handler. Payload: offset_i24 (bits 8-31)
  PushHandler,
  /// PopHandler: pop exception handler
  PopHandler,

  /// IteratorNext: rd holds the iterator and receives the iterator result.
  IteratorNext,
  /// IteratorClose: close the iterator held in rd.
  IteratorClose,
  /// Yield: yield rd from generator
  Yield,
  /// YieldStar: delegate yield for yield*; aux8 is 1 for async delegation.
  YieldStar,

  /// Return rd
  Return,
  /// PushWithEnv: push with() environment
  PushWithEnv,
  /// PopEnv: pop lexical environment
  PopEnv,
  /// AwaitPromise: await the promise/value held in rd and write the result to rd.
  AwaitPromise,
  /// SuperConstruct: super(...) in a derived constructor. Aux: argument count (u16).
  SuperConstruct,
  /// GeneratorInitDone: marker for end of generator prologue
  GeneratorInitDone,

  /// GetModuleExport: rd = modules[descriptor.module].exports[descriptor.name].
  /// Aux: descriptor constant index (u16).
  GetModuleExport,
  /// SetModuleExport: modules[descriptor.module].exports[descriptor.name] = rd.
  /// Aux: descriptor constant index (u16).
  SetModuleExport,

  /// CloneLocalCell: clone a local cell for closure capture. Aux: slot index (u16)
  CloneLocalCell,
  /// SetSuperProperty: super[rs] = aux register, using the receiver/base in rd.
  SetSuperProperty,
  /// DeleteIdentifier: delete the identifier name held in rd.
  DeleteIdentifier,
  /// GetBuiltin: rd = builtin[name]. Aux: name constant index (u16)
  GetBuiltin,

  /// InstanceOf: rd = rs1 instanceof rs2
  InstanceOf,
  /// Spread: call the callee in rd with values in rs expanded by the aux mask register.
  Spread,
  /// ResetLocalCell: create a fresh uninitialized lexical cell. Aux: slot index (u16)
  ResetLocalCell,
  /// Load a spill slot into `rd`. Aux: spill index (u16).
  LoadSpill,
  /// Store `rd` into a spill slot. Aux: spill index (u16).
  StoreSpill,
  /// rd = ~rs (bitwise NOT)
  BitNot,
  /// GetAsyncIterator: acquire an async iterator record for rd.
  GetAsyncIterator,
  /// Increment: apply ToNumeric and add the matching numeric unit.
  Increment,
  /// Decrement: apply ToNumeric and subtract the matching numeric unit.
  Decrement,
  /// PushLexicalEnv: create a declarative environment for the local slot in
  /// aux16. `rd` contains the binding name.
  PushLexicalEnv,
}

/// Total number of opcode variants.
pub const OP_COUNT: usize = Opcode::PushLexicalEnv as usize + 1;

impl Opcode {
  /// Converts a byte to an Opcode if valid, returns None otherwise
  pub fn from_byte(b: u8) -> Option<Opcode> {
    Opcode::try_from(b).ok()
  }

  #[inline(always)]
  pub(crate) unsafe fn from_byte_unchecked(b: u8) -> Opcode {
    debug_assert!(Self::from_byte(b).is_some());
    unsafe { std::mem::transmute::<u8, Opcode>(b) }
  }

  /// Returns the byte representation of this opcode.
  pub fn to_byte(self) -> u8 {
    self as u8
  }

  /// Returns the human-readable name of this opcode.
  pub fn name(&self) -> &'static str {
    match self {
      Opcode::Const => "Const",
      Opcode::Null => "Null",
      Opcode::True => "True",
      Opcode::False => "False",
      Opcode::Undefined => "Undefined",
      Opcode::LoadLocal => "LoadLocal",
      Opcode::StoreLocal => "StoreLocal",
      Opcode::Move => "Move",
      Opcode::Add => "Add",
      Opcode::Sub => "Sub",
      Opcode::Mul => "Mul",
      Opcode::Div => "Div",
      Opcode::Mod => "Mod",
      Opcode::Pow => "Pow",
      Opcode::Neg => "Neg",
      Opcode::BitAnd => "BitAnd",
      Opcode::BitOr => "BitOr",
      Opcode::BitXor => "BitXor",
      Opcode::Shl => "Shl",
      Opcode::Shr => "Shr",
      Opcode::Ushr => "Ushr",
      Opcode::CompareJump => "CompareJump",
      Opcode::Jump => "Jump",
      Opcode::JumpIfTrue => "JumpIfTrue",
      Opcode::JumpIfFalse => "JumpIfFalse",
      Opcode::JumpIfNullish => "JumpIfNullish",
      Opcode::And => "And",
      Opcode::Or => "Or",
      Opcode::Not => "Not",
      Opcode::Typeof => "Typeof",
      Opcode::GetProperty => "GetProperty",
      Opcode::SetProperty => "SetProperty",
      Opcode::DeleteProperty => "DeleteProperty",
      Opcode::HasProperty => "HasProperty",
      Opcode::GetGlobal => "GetGlobal",
      Opcode::TypeofGlobal => "TypeofGlobal",
      Opcode::SetGlobal => "SetGlobal",
      Opcode::GetUpvalue => "GetUpvalue",
      Opcode::SetUpvalue => "SetUpvalue",
      Opcode::Call => "Call",
      Opcode::TailCall => "TailCall",
      Opcode::Construct => "Construct",
      Opcode::CallMethod => "CallMethod",
      Opcode::MakeClosure => "MakeClosure",
      Opcode::GetThis => "GetThis",
      Opcode::GetNewTarget => "GetNewTarget",
      Opcode::GetSuperProperty => "GetSuperProperty",
      Opcode::BuildArray => "BuildArray",
      Opcode::BuildObject => "BuildObject",
      Opcode::ArrayFromSpread => "ArrayFromSpread",
      Opcode::Throw => "Throw",
      Opcode::PushHandler => "PushHandler",
      Opcode::PopHandler => "PopHandler",
      Opcode::IteratorNext => "IteratorNext",
      Opcode::IteratorClose => "IteratorClose",
      Opcode::Yield => "Yield",
      Opcode::YieldStar => "YieldStar",
      Opcode::Return => "Return",
      Opcode::PushWithEnv => "PushWithEnv",
      Opcode::PopEnv => "PopEnv",
      Opcode::AwaitPromise => "AwaitPromise",
      Opcode::SuperConstruct => "SuperConstruct",
      Opcode::GeneratorInitDone => "GeneratorInitDone",
      Opcode::GetModuleExport => "GetModuleExport",
      Opcode::SetModuleExport => "SetModuleExport",
      Opcode::CloneLocalCell => "CloneLocalCell",
      Opcode::SetSuperProperty => "SetSuperProperty",
      Opcode::DeleteIdentifier => "DeleteIdentifier",
      Opcode::GetBuiltin => "GetBuiltin",
      Opcode::InstanceOf => "InstanceOf",
      Opcode::Spread => "Spread",
      Opcode::ResetLocalCell => "ResetLocalCell",
      Opcode::LoadSpill => "LoadSpill",
      Opcode::StoreSpill => "StoreSpill",
      Opcode::BitNot => "BitNot",
      Opcode::GetAsyncIterator => "GetAsyncIterator",
      Opcode::Increment => "Increment",
      Opcode::Decrement => "Decrement",
      Opcode::PushLexicalEnv => "PushLexicalEnv",
    }
  }

  /// Returns whether this opcode is a jump (conditional or unconditional).
  pub fn is_jump(&self) -> bool {
    matches!(
      self,
      Opcode::CompareJump
        | Opcode::Jump
        | Opcode::JumpIfTrue
        | Opcode::JumpIfFalse
        | Opcode::JumpIfNullish
    )
  }

  /// Returns whether this opcode is an arithmetic operation.
  pub fn is_arithmetic(&self) -> bool {
    matches!(
      self,
      Opcode::Add
        | Opcode::Sub
        | Opcode::Mul
        | Opcode::Div
        | Opcode::Mod
        | Opcode::Pow
        | Opcode::Neg
    )
  }

  /// Returns whether this opcode is a comparison operation.
  pub fn is_comparison(&self) -> bool {
    matches!(self, Opcode::CompareJump)
  }

  /// Returns whether this opcode is a logical or boolean operator.
  pub fn is_logical(&self) -> bool {
    matches!(self, Opcode::And | Opcode::Or | Opcode::Not)
  }
}

impl TryFrom<u8> for Opcode {
  type Error = ();

  /// Converts a u8 value into an Opcode variant. Returns an error if the value is out of range.
  fn try_from(value: u8) -> Result<Self, Self::Error> {
    use Opcode::*;
    match value {
      0 => Ok(Const),
      1 => Ok(Null),
      2 => Ok(True),
      3 => Ok(False),
      4 => Ok(Undefined),
      5 => Ok(LoadLocal),
      6 => Ok(StoreLocal),
      7 => Ok(Move),
      8 => Ok(Add),
      9 => Ok(Sub),
      10 => Ok(Mul),
      11 => Ok(Div),
      12 => Ok(Mod),
      13 => Ok(Pow),
      14 => Ok(Neg),
      15 => Ok(BitAnd),
      16 => Ok(BitOr),
      17 => Ok(BitXor),
      18 => Ok(Shl),
      19 => Ok(Shr),
      20 => Ok(Ushr),
      21 => Ok(CompareJump),
      22 => Ok(Jump),
      23 => Ok(JumpIfTrue),
      24 => Ok(JumpIfFalse),
      25 => Ok(JumpIfNullish),
      26 => Ok(And),
      27 => Ok(Or),
      28 => Ok(Not),
      29 => Ok(Typeof),
      30 => Ok(GetProperty),
      31 => Ok(SetProperty),
      32 => Ok(DeleteProperty),
      33 => Ok(HasProperty),
      34 => Ok(GetGlobal),
      35 => Ok(TypeofGlobal),
      36 => Ok(SetGlobal),
      37 => Ok(GetUpvalue),
      38 => Ok(SetUpvalue),
      39 => Ok(Call),
      40 => Ok(TailCall),
      41 => Ok(Construct),
      42 => Ok(CallMethod),
      43 => Ok(MakeClosure),
      44 => Ok(GetThis),
      45 => Ok(GetNewTarget),
      46 => Ok(GetSuperProperty),
      47 => Ok(BuildArray),
      48 => Ok(BuildObject),
      49 => Ok(ArrayFromSpread),
      54 => Ok(Throw),
      55 => Ok(PushHandler),
      56 => Ok(PopHandler),
      57 => Ok(IteratorNext),
      58 => Ok(IteratorClose),
      59 => Ok(Yield),
      60 => Ok(YieldStar),
      61 => Ok(Return),
      62 => Ok(PushWithEnv),
      63 => Ok(PopEnv),
      64 => Ok(AwaitPromise),
      65 => Ok(SuperConstruct),
      66 => Ok(GeneratorInitDone),
      67 => Ok(GetModuleExport),
      68 => Ok(SetModuleExport),
      69 => Ok(CloneLocalCell),
      70 => Ok(SetSuperProperty),
      71 => Ok(DeleteIdentifier),
      72 => Ok(GetBuiltin),
      73 => Ok(InstanceOf),
      74 => Ok(Spread),
      75 => Ok(ResetLocalCell),
      76 => Ok(LoadSpill),
      77 => Ok(StoreSpill),
      78 => Ok(BitNot),
      79 => Ok(GetAsyncIterator),
      80 => Ok(Increment),
      81 => Ok(Decrement),
      82 => Ok(PushLexicalEnv),
      _ => Err(()),
    }
  }
}

impl From<Opcode> for u8 {
  fn from(op: Opcode) -> Self {
    op as u8
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::value::ObjectData;
  use std::cell::RefCell;
  use std::rc::Rc;

  #[test]
  fn compiled_code_are_sendable_between_threads() {
    fn assert_send<T: Send>() {}
    assert_send::<Chunk>();
  }

  #[test]
  fn runtime_owned_values_are_rejected_as_constants() {
    let object = Value::Object(Rc::new(RefCell::new(ObjectData::new())));
    assert_eq!(Constant::try_from(object), Err(ConstantConversionError));
  }

  #[test]
  fn op_round_trip() {
    assert_eq!(OP_COUNT, Opcode::PushLexicalEnv as usize + 1);
    for i in 0u8..=Opcode::PushLexicalEnv as u8 {
      let Ok(op) = Opcode::try_from(i) else {
        assert!((50..=53).contains(&i), "unexpected opcode gap at {i}");
        continue;
      };
      let b = u8::from(op);
      assert_eq!(b, i);
      let op2 = Opcode::from_byte(b).expect("from_byte");
      assert_eq!(op2, op);
    }
    for retired in 50..=53 {
      assert!(Opcode::from_byte(retired).is_none());
    }
  }

  #[test]
  fn chunks_have_a_fixed_v2_version() {
    let code = Chunk::default();
    assert_eq!(code.version, BytecodeVersion::RiscV2);
    assert_eq!(code.version as u8, 2);
  }

  #[test]
  fn verifier_rejects_invalid_opcode_and_branch_target() {
    let mut invalid_opcode = Chunk::default();
    invalid_opcode.write_instr(0xFF, 1);
    assert!(
      invalid_opcode
        .verify()
        .unwrap_err()
        .contains("invalid opcode")
    );

    let mut invalid_jump = Chunk::default();
    invalid_jump.write_instr(Opcode::Jump as u32 | (10 << 8), 1);
    assert!(
      invalid_jump
        .verify()
        .unwrap_err()
        .contains("out-of-bounds instruction")
    );
  }

  #[test]
  fn verifier_rejects_invalid_operands_and_metadata() {
    let mut invalid_global = Chunk::default();
    invalid_global
      .constants
      .push(Constant::Number(1.0));
    invalid_global.write_instr(encode_aux16_instruction(Opcode::GetGlobal, 0, 0), 1);
    assert!(
      invalid_global
        .verify()
        .unwrap_err()
        .contains("must contain a string")
    );

    let mut invalid_local = Chunk {
      root_slot_types: vec![None],
      root_slot_names: vec![Some("value".into())],
      root_lexical_tdz: vec![false],
      root_slot_immutable: vec![false],
      ..Chunk::default()
    };
    invalid_local.write_instr(encode_aux16_instruction(Opcode::LoadLocal, 0, 1), 1);
    assert!(
      invalid_local
        .verify()
        .unwrap_err()
        .contains("local slot 1")
    );

    let mut invalid_spill = Chunk {
      spill_slot_count: 1,
      ..Chunk::default()
    };
    invalid_spill.write_instr(encode_aux16_instruction(Opcode::LoadSpill, 0, 1), 1);
    assert!(
      invalid_spill
        .verify()
        .unwrap_err()
        .contains("spill slot 1")
    );

    let mut invalid_window = Chunk {
      spill_slot_count: 2,
      value_spill_slot_count: 1,
      ..Chunk::default()
    };
    invalid_window.write_instr(encode_aux16_instruction(Opcode::BuildArray, 0, 2), 1);
    assert!(
      invalid_window
        .verify()
        .unwrap_err()
        .contains("operand window")
    );

    let mut invalid_finally = Chunk::default();
    invalid_finally.write_instr(Opcode::Undefined as u32, 1);
    invalid_finally
      .finally_handlers
      .push(FinallyHandler {
        push_pc: 0,
        finalizer_pc: 0,
        return_slot: 0,
        did_return_slot: 0,
      });
    assert!(
      invalid_finally
        .verify()
        .unwrap_err()
        .contains("finally handler push")
    );
  }

  #[test]
  fn decoded_branch_offsets_follow_layouts() {
    let compare_word = encode_compare_jump(2, 3, 4, -3).expect("compare jump");
    let instruction = Instruction {
      opcode: Opcode::CompareJump,
      payload: compare_word >> 8,
    };
    assert_eq!(instruction.branch_offset(), Some(-3));

    let conditional_word =
      encode_conditional_branch(Opcode::JumpIfFalse, 7, -42).expect("conditional jump");
    let instruction = Instruction {
      opcode: Opcode::JumpIfFalse,
      payload: conditional_word >> 8,
    };
    assert_eq!(instruction.branch_offset(), Some(-42));
  }

  #[test]
  fn compare_jump_encoding_round_trips() {
    let word = encode_compare_jump(6, 3, 14, -4096).expect("encodable branch");
    assert_eq!(decode_compare_jump(word), (6, 3, 14, -4096));
    assert!(encode_compare_jump(0, 0, 0, 4096).is_none());
  }

  #[test]
  fn verifier_uses_conditional_branch_layout() {
    let mut code = Chunk::default();
    code.write_instr(
      encode_conditional_branch(Opcode::JumpIfTrue, 15, 1).expect("conditional jump"),
      1,
    );
    code.write_instr(Opcode::Undefined as u32, 1);
    code.write_instr(Opcode::Return as u32, 1);
    assert!(code.verify().is_ok());

    code.words[0] =
      encode_conditional_branch(Opcode::JumpIfTrue, 15, 10).expect("conditional jump");
    assert!(
      code
        .verify()
        .unwrap_err()
        .contains("out-of-bounds instruction")
    );
  }

  #[test]
  fn register_abi_has_conventional_call_roles() {
    assert_eq!(abi::RETURN, Register::R0);
    assert_eq!(abi::RECEIVER, Register::R1);
    assert_eq!(abi::REGISTER_ARGUMENT_COUNT, 6);
    assert_eq!(
      (0..abi::REGISTER_ARGUMENT_COUNT)
        .map(|index| abi::argument_register(index).expect("register argument"))
        .collect::<Vec<_>>(),
      [
        Register::R2,
        Register::R3,
        Register::R4,
        Register::R5,
        Register::R6,
        Register::R7,
      ]
    );
    assert_eq!(abi::argument_register(abi::REGISTER_ARGUMENT_COUNT), None);
    assert!(abi::is_argument_register(Register::R2));
    assert!(abi::is_argument_register(Register::R7));
    assert!(abi::is_temporary_register(Register::R8));
    assert!(abi::is_temporary_register(Register::R15));
    assert!(!abi::is_temporary_register(Register::R7));
  }
}
