// SPDX-License-Identifier: MPL-2.0
//! Bytecode representation and VM instruction set.
//!
//! This module defines the bytecode format used by the VM, including:
//! - [`Op`] enum: All VM opcodes with safe conversions to/from `u8`
//! - [`Chunk`] struct: Container for bytecode, constants, and function prototypes
//! - [`FunctionProto`] struct: Metadata for compiled functions
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
//! - RISC CompareJump: cond(3) | rs1(4) | rs2(4) | signed offset(13)
//!
//! # Example
//!
//! ```
//! use denko::bytecode::{Chunk, Op};
//! use std::sync::Arc;
//!
//! let mut chunk = Chunk::new(Arc::new(String::new()));
//! chunk.write_instr(Op::Const as u32, 1); // constant index 0
//! ```

use crate::typecheck::Type;
use crate::value::tagged::TaggedValue;
use crate::value::{SymbolId, Value};
use js_string::JsString;
use num_bigint::BigInt;
use oxc_span::Span;
use std::convert::TryFrom;
use std::sync::Arc;

/// Description of a captured binding for a closure.
#[derive(Clone, Debug)]
pub struct UpvalueSpec {
  /// True when capturing from the immediately enclosing function's locals;
  /// false when capturing from the enclosing function's own upvalue list.
  pub is_local: bool,
  /// Slot or upvalue index in the enclosing function.
  pub index: u8,
  /// Optional global binding name that should mirror writes to this captured
  /// cell. Used for root `var` bindings that are also exposed on the global
  /// object.
  pub mirror_global_name: Option<String>,
  /// Whether this captured binding is immutable (`const`/function-name binding).
  pub immutable: bool,
  /// Whether writes in non-strict mode should be silently ignored.
  pub sloppy_immutable_silent: bool,
}

/// Version marker carried by every bytecode chunk.
///
/// Blitz has a single executable bytecode contract. The explicit marker is
/// retained so serialized chunks can be rejected cleanly if the contract is
/// changed in the future.
#[repr(u8)]
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum BytecodeVersion {
  /// Fixed-width encoding interpreted with the 16-register RISC v2 semantics.
  #[default]
  RiscV2 = 2,
}

/// A decoded fixed-width instruction.
#[derive(Clone, Copy, Debug)]
pub struct DecodedInstruction {
  /// Decoded opcode.
  pub op: Op,
  /// Unsigned 24-bit payload.
  pub payload: u32,
}

impl DecodedInstruction {
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
    match self.op {
      Op::CompareJump => {
        let word = self.op as u32 | ((self.payload & 0x00FF_FFFF) << 8);
        Some(decode_compare_jump(word).3 as i32)
      }
      Op::JumpIfFalse | Op::JumpIfTrue | Op::JumpIfNullish => {
        let word = self.op as u32 | ((self.payload & 0x00FF_FFFF) << 8);
        Some(decode_risc_cond_offset(word))
      }
      Op::Jump | Op::PushHandler => Some(self.operand_i24()),
      _ => None,
    }
  }
}

/// An immutable literal stored in compiled bytecode.
///
/// Runtime-owned objects are deliberately excluded so compiled chunks can be
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
  /// Return the compact representation for a non-heap constant.
  ///
  /// This is the constant-pool boundary used by the staged register/JIT
  /// migration. Heap-backed literals continue through [`Self::to_value`] until
  /// the VM-owned heap stores their payloads.
  #[allow(dead_code, reason = "used by the staged compact-register migration")]
  pub(crate) fn as_tagged_immediate(&self) -> Option<TaggedValue<'static>> {
    match self {
      Self::Uninitialized => Some(TaggedValue::UNINITIALIZED),
      Self::Number(value) => Some(TaggedValue::number(*value)),
      Self::Bool(value) => Some(TaggedValue::boolean(*value)),
      Self::Undefined => Some(TaggedValue::UNDEFINED),
      Self::Null => Some(TaggedValue::NULL),
      Self::String(_) | Self::BigInt(_) | Self::Symbol(..) => None,
    }
  }

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

/// A chunk of bytecode containing opcodes, constants, and metadata
#[derive(Clone, Debug)]
pub struct Chunk {
  /// Version of the encoded instruction words.
  pub version: BytecodeVersion,
  /// Instructions stored as fixed-width 32-bit words. Each u32 contains
  /// opcode in the low 8 bits and a 24-bit payload in the upper bits.
  pub code: Vec<u32>,
  /// Constant pool values
  pub constants: Vec<Constant>,
  /// Function prototypes
  pub functions: Vec<FunctionProto>,
  /// Line number information for debugging
  pub lines: Vec<u32>,
  /// Original source text for this chunk
  pub source_text: Arc<String>,
  /// Root script slot type annotations.
  pub root_slot_types: Vec<Option<Type>>,
  /// Root script binding names.
  pub root_slot_names: Vec<Option<String>>,
  /// Root script temporal dead zone flags.
  pub root_lexical_tdz: Vec<bool>,
  /// Root script immutability flags (`const`).
  pub root_slot_immutable: Vec<bool>,
  /// Number of non-lexical frame spill slots required by RISC v2 lowering.
  pub risc_spill_count: u16,
  /// Prefix of `risc_spill_count` occupied by stable lowered value homes.
  pub risc_value_spill_count: u16,
  /// Whether the compiled root source is strict.
  pub root_strict: bool,
  /// Whether `new.target` is valid in this chunk's execution context.
  pub allow_new_target: bool,
  /// Whether `super` property access/calls are valid in this chunk.
  pub allow_super: bool,
  /// Finalizer entry points used to inject generator `return` completions.
  pub finally_handlers: Vec<FinallyHandler>,
}

#[derive(Clone, Debug)]
/// Control-flow metadata for a protected region with a `finally` clause.
pub struct FinallyHandler {
  /// Instruction that installs the protected region's exception handler.
  pub push_ip: usize,
  /// First instruction in the normal finalizer body.
  pub finalizer_ip: usize,
  /// Local slot that carries an injected generator return value.
  pub return_slot: usize,
  /// Local slot indicating that the finalizer must complete with a return.
  pub did_return_slot: usize,
}

#[derive(Clone, Debug)]
/// Function prototype containing metadata for compiled functions
pub struct FunctionProto {
  /// Function name
  pub name: String,
  /// Source span in the original script
  pub span: Span,
  /// Number of parameters
  pub arity: usize,
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
  /// Creates a new empty chunk
  pub fn new(source_text: Arc<String>) -> Self {
    Self {
      version: BytecodeVersion::RiscV2,
      code: Vec::new(),
      constants: Vec::new(),
      functions: Vec::new(),
      lines: Vec::new(),
      source_text,
      root_slot_types: Vec::new(),
      root_slot_names: Vec::new(),
      root_lexical_tdz: Vec::new(),
      root_slot_immutable: Vec::new(),
      risc_spill_count: 0,
      risc_value_spill_count: 0,
      root_strict: false,
      allow_new_target: false,
      allow_super: false,
      finally_handlers: Vec::new(),
    }
  }

  /// Decode one RISC v2 instruction word.
  pub fn decode_at(
    &self,
    ip: usize,
  ) -> Result<DecodedInstruction, String> {
    let word = self
      .code
      .get(ip)
      .copied()
      .ok_or_else(|| format!("instruction index {} is out of bounds", ip))?;
    let opcode = (word & 0xFF) as u8;
    let op = Op::from_byte(opcode)
      .ok_or_else(|| format!("invalid opcode {} at instruction {}", opcode, ip))?;
    Ok(DecodedInstruction {
      op,
      payload: word >> 8,
    })
  }

  /// Format one instruction for tracing and diagnostics.
  pub fn disassemble_at(
    &self,
    ip: usize,
  ) -> Result<String, String> {
    let instruction = self.decode_at(ip)?;
    let word = self.code[ip];
    let detail = match instruction.op {
      Op::CompareJump => {
        let (condition, left, right, offset) = decode_compare_jump(word);
        format!(
          "compare={} r{} r{} offset={}",
          condition, left, right, offset
        )
      }
      Op::JumpIfFalse | Op::JumpIfTrue | Op::JumpIfNullish => {
        format!(
          "r{} offset={}",
          decode_rd(word),
          decode_risc_cond_offset(word)
        )
      }
      Op::Jump | Op::PushHandler => {
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
      Ok(format!("{:#06} {}", ip, instruction.op.name()))
    } else {
      Ok(format!("{:#06} {} {}", ip, instruction.op.name(), detail))
    }
  }

  /// Validate structural invariants before a chunk is executed.
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
    if self.lines.len() != self.code.len() {
      return Err(format!(
        "line table length {} does not match instruction count {}",
        self.lines.len(),
        self.code.len()
      ));
    }
    if self.risc_value_spill_count > self.risc_spill_count {
      return Err(format!(
        "RISC value spill count {} exceeds total spill count {}",
        self.risc_value_spill_count, self.risc_spill_count
      ));
    }

    for ip in 0..self.code.len() {
      let instruction = self.decode_at(ip)?;
      let word = self.code[ip];
      if let Some(offset) = instruction.branch_offset() {
        let target = ip as i64 + 1 + offset as i64;
        if !(0..self.code.len() as i64).contains(&target) {
          return Err(format!(
            "jump at instruction {} targets out-of-bounds instruction {}",
            ip, target
          ));
        }
      }

      let operand = decode_risc_aux16(word) as usize;
      match instruction.op {
        Op::Const => self.verify_constant_index(ip, operand, false)?,
        Op::GetGlobal | Op::TypeofGlobal | Op::SetGlobal | Op::GetBuiltin => {
          self.verify_constant_index(ip, operand, true)?
        }
        Op::GetModuleExport | Op::SetModuleExport => {
          self.verify_module_descriptor(ip, operand)?;
        }
        Op::MakeClosure if operand >= self.functions.len() => {
          return Err(format!(
            "function index {operand} at instruction {ip} is out of bounds"
          ));
        }
        Op::LoadLocal | Op::StoreLocal | Op::CloneLocalCell | Op::ResetLocalCell
          if operand >= local_count =>
        {
          return Err(format!(
            "local slot {operand} at instruction {ip} is out of bounds"
          ));
        }
        Op::GetUpvalue | Op::SetUpvalue if operand >= upvalue_count => {
          return Err(format!(
            "upvalue slot {operand} at instruction {ip} is out of bounds"
          ));
        }
        Op::LoadSpill | Op::StoreSpill if operand >= self.risc_spill_count as usize => {
          return Err(format!(
            "spill slot {operand} at instruction {ip} is out of bounds"
          ));
        }
        Op::Call | Op::TailCall | Op::Construct | Op::CallBuiltin => {
          if decode_rd(word) != 0 || decode_rs(word) != 0 {
            return Err(format!("call ABI registers at instruction {ip} must be r0"));
          }
          self.verify_operand_window(ip, operand.saturating_sub(6))?;
        }
        Op::BuildArray | Op::SuperConstruct => {
          self.verify_operand_window(ip, operand)?;
        }
        Op::BuildObject => {
          let required = operand
            .checked_mul(2)
            .ok_or_else(|| format!("object operand window at instruction {ip} overflows"))?;
          self.verify_operand_window(ip, required)?;
        }
        Op::SetProperty | Op::SetSuperProperty | Op::Spread
          if decode_aux8(word) > 15 || decode_aux8_2(word) != 0 =>
        {
          return Err(format!(
            "extra register operand at instruction {ip} is malformed"
          ));
        }
        _ => {}
      }
    }

    for (index, handler) in self.finally_handlers.iter().enumerate() {
      if self.finally_handlers[..index]
        .iter()
        .any(|other| other.push_ip == handler.push_ip)
      {
        return Err(format!(
          "finally handler push instruction {} is duplicated",
          handler.push_ip
        ));
      }
      if self
        .code
        .get(handler.push_ip)
        .and_then(|word| Op::from_byte(opcode_from_instr(*word)))
        != Some(Op::PushHandler)
      {
        return Err(format!(
          "finally handler push instruction {} is invalid",
          handler.push_ip
        ));
      }
      if handler.finalizer_ip >= self.code.len() {
        return Err(format!(
          "finally handler target {} is out of bounds",
          handler.finalizer_ip
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
    ip: usize,
    index: usize,
    require_string: bool,
  ) -> Result<(), String> {
    let constant = self
      .constants
      .get(index)
      .ok_or_else(|| format!("constant index {index} at instruction {ip} is out of bounds"))?;
    if require_string && !matches!(constant, Constant::String(_)) {
      return Err(format!(
        "constant index {index} at instruction {ip} must contain a string"
      ));
    }
    Ok(())
  }

  fn verify_module_descriptor(
    &self,
    ip: usize,
    index: usize,
  ) -> Result<(), String> {
    self.verify_constant_index(ip, index, true)?;
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
    ip: usize,
    required: usize,
  ) -> Result<(), String> {
    let available = usize::from(self.risc_spill_count - self.risc_value_spill_count);
    if required > available {
      return Err(format!(
        "operand window of {required} values at instruction {ip} exceeds {available} slots"
      ));
    }
    Ok(())
  }

  /// Writes a packed instruction word to the chunk with line number info.
  /// The instruction layout: low 8 bits = opcode, upper 24 bits = payload.
  pub fn write_instr(
    &mut self,
    instr: u32,
    line: u32,
  ) {
    self.code.push(instr);
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
    proto: FunctionProto,
  ) -> usize {
    self.functions.push(proto);
    self.functions.len() - 1
  }

  // --- Helpers for reading/writing 16-bit operands (little-endian) ---

  /// Read the 24-bit signed payload (bits 8..31) from instruction at `idx`.
  /// Returns None if out of bounds.
  pub fn read_i24_payload_at(
    &self,
    idx: usize,
  ) -> Option<i32> {
    let w = *self.code.get(idx)?;
    // interpret upper 24 bits as signed i32 with sign extension
    let payload = (w >> 8_i32) & 0x00FF_FFFF;
    // sign-extend 24-bit to 32-bit signed
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
    if idx >= self.code.len() {
      return false;
    }
    // ensure fits in signed 24-bit
    if !(-(1_i32 << 23_i32)..(1_i32 << 23_i32)).contains(&val) {
      return false;
    }
    let instr = self.code[idx];
    // clear upper 24 bits then or in new payload
    let payload_u32 = (val.cast_unsigned() & 0x00FF_FFFF) << 8_u32;
    let new = (instr & 0x0000_00FF) | payload_u32;
    self.code[idx] = new;
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

  /// Return the instruction-index target of a RISC branch at `ip`.
  pub fn jump_target_from_ip(
    &self,
    ip: usize,
  ) -> Option<usize> {
    let instr = *self.code.get(ip)?;
    let op_byte = opcode_from_instr(instr);
    let op = Op::from_byte(op_byte)?;
    if !op.is_jump() {
      return None;
    }
    let offset = self.decode_at(ip).ok()?.branch_offset()?;
    let next = ip as i64 + 1;
    let target = next.checked_add(offset as i64)?;
    if target < 0 || target >= (self.code.len() as i64) {
      return None;
    }
    usize::try_from(target).ok()
  }
}

// ============================================================================
// RISC Bytecode: Register Operand Encoding/Decoding Helpers
// ============================================================================

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
pub fn encode_registers(
  rd: u8,
  rs: u8,
) -> u32 {
  encode_rd(rd) | encode_rs(rs)
}

/// Encode a normal RISC instruction with destination and source registers.
pub fn encode_risc_registers(
  op: Op,
  rd: u8,
  rs: u8,
) -> u32 {
  op as u32 | encode_registers(rd, rs)
}

/// Encode a RISC instruction with `rd` and a 16-bit auxiliary operand.
pub fn encode_risc_aux16(
  op: Op,
  rd: u8,
  aux: u16,
) -> u32 {
  op as u32 | encode_rd(rd) | ((aux as u32) << 16)
}

/// Decode the 16-bit auxiliary operand from a normal RISC instruction.
pub fn decode_risc_aux16(instr: u32) -> u16 {
  (instr >> 16) as u16
}

/// Encode a RISC conditional jump using `rd` and a signed 20-bit offset.
pub fn encode_risc_cond_jump(
  op: Op,
  rd: u8,
  offset: i32,
) -> Option<u32> {
  if !matches!(op, Op::JumpIfFalse | Op::JumpIfTrue | Op::JumpIfNullish)
    || !(-(1 << 19)..(1 << 19)).contains(&offset)
  {
    return None;
  }
  Some(op as u32 | encode_rd(rd) | (((offset as u32) & 0x000F_FFFF) << 12))
}

/// Decode the signed 20-bit offset from a RISC conditional jump.
pub fn decode_risc_cond_offset(instr: u32) -> i32 {
  let value = (instr >> 12) & 0x000F_FFFF;
  if value & 0x0008_0000 != 0 {
    (value | 0xFFF0_0000) as i32
  } else {
    value as i32
  }
}

/// Pack a RISC compare-and-branch instruction.
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
  Some(Op::CompareJump as u32 | (payload << 8))
}

/// Decode a RISC compare-and-branch word as `(condition, left, right, offset)`.
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

// ============================================================================

/// Returns the opcode from `instr`.
#[inline]
pub fn opcode_from_instr(instr: u32) -> u8 {
  (instr & 0xFF) as u8
}

/// Returns the `idx`th payload byte from `instr`, where `idx` is 0-based.
#[inline]
pub fn payload_byte(
  instr: u32,
  idx: usize,
) -> u8 {
  // idx: 0 -> first payload byte (bits 8..15), 1 -> bits 16..23, 2 -> bits 24..31
  if idx > 2 {
    0
  } else {
    ((instr >> (8 * (idx + 1))) & 0xFF) as u8
  }
}

impl FunctionProto {
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

// ----------------------------------------------------------------------------
// Opcode definitions
// ----------------------------------------------------------------------------

#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq)]
/// Bytecode operation codes for the RISC v2 format.
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
/// NOTE: The numeric discriminant of each `Op` variant is part of the runtime
/// bytecode format. Do not reorder or insert variants in the middle without
/// defining a new bytecode version and corresponding compatibility handling.
pub enum Op {
  // === Constants & Immediate Values (5) ===
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

  // === Register/Local Operations (3) ===
  /// Load local slot → rd. Aux: slot index (u16)
  LoadLocal,
  /// Store rd → local slot. Aux: slot index (u16)
  StoreLocal,
  /// Move rs → rd (copy register)
  Move,

  // === Arithmetic 2-register format: rd = rd op rs (7) ===
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

  // === Bitwise 2-register format: rd = rd op rs (5) ===
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

  // === Comparison & Conditional Jumps (5) ===
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

  // === Logical (4) ===
  /// Combine operand truthiness: rd = truthy(rd) && truthy(rs).
  And,
  /// Combine operand truthiness: rd = truthy(rd) || truthy(rs).
  Or,
  /// rd = !rs (logical NOT)
  Not,
  /// rd = typeof rs
  Typeof,

  // === Property Access (4) ===
  /// rd = rd[rs]. The property-get cache is keyed by instruction position.
  GetProperty,
  /// rd[rs] = aux register. The value register is stored in the first aux byte.
  SetProperty,
  /// rd = delete obj[key]
  DeleteProperty,
  /// rd = (key in obj)
  HasProperty,

  // === Global Variable Access (3) ===
  /// rd = GetGlobal(name_const_idx). Aux: name constant index (u16)
  GetGlobal,
  /// rd = typeof name_const_idx. Aux: name constant index (u16)
  TypeofGlobal,
  /// SetGlobal(name_const_idx, rd). Aux: name constant index (u16)
  SetGlobal,

  // === Upvalue Access (2) ===
  /// rd = GetUpvalue(upval_idx). Aux: upvalue index (u16)
  GetUpvalue,
  /// SetUpvalue(upval_idx, rd). Aux: upvalue index (u16)
  SetUpvalue,

  // === Function Calls (4) ===
  /// Call the callee in r0 with arguments in r2-r7 and then the spill window.
  /// The result is written to r0. Aux: argument count (u16).
  Call,
  /// Tail-call form using the same operand convention as `Call`.
  TailCall,
  /// Construct: new r0(...). Aux: argument count (u16)
  Construct,
  /// CallBuiltin: call the builtin in r0 with receiver r1 and Call's arguments.
  CallBuiltin,

  // === Closures & this/target (4) ===
  /// rd = MakeClosure(proto_idx). Aux: prototype index (u16)
  MakeClosure,
  /// rd = GetThis
  GetThis,
  /// rd = GetNewTarget
  GetNewTarget,
  /// rd = super[rs], using the receiver/base value in rd.
  GetSuperProperty,

  // === Array & Object Construction (3) ===
  /// BuildArray: collect values from the spill window into rd. Aux: element count (u16)
  BuildArray,
  /// BuildObject: collect key/value pairs from the spill window into rd. Aux: pair count (u16)
  BuildObject,
  /// ArrayFromSpread: build array with spread
  ArrayFromSpread,

  // === Exception Handling (3) ===
  /// Throw rd
  Throw = 54,
  /// PushHandler: push exception handler. Payload: offset_i24 (bits 8-31)
  PushHandler,
  /// PopHandler: pop exception handler
  PopHandler,

  // === Iteration (4) ===
  /// IteratorNext: rd holds the iterator and receives the iterator result.
  IteratorNext,
  /// IteratorClose: close the iterator held in rd.
  IteratorClose,
  /// Yield: yield rd from generator
  Yield,
  /// YieldStar: delegate yield for yield*
  YieldStar,

  // === Special (6) ===
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

  // === Module System (2) ===
  /// GetModuleExport: rd = modules[descriptor.module].exports[descriptor.name].
  /// Aux: descriptor constant index (u16).
  GetModuleExport,
  /// SetModuleExport: modules[descriptor.module].exports[descriptor.name] = rd.
  /// Aux: descriptor constant index (u16).
  SetModuleExport,

  // === Semantic Operations (4) ===
  /// CloneLocalCell: clone a local cell for closure capture. Aux: slot index (u16)
  CloneLocalCell,
  /// SetSuperProperty: super[rs] = aux register, using the receiver/base in rd.
  SetSuperProperty,
  /// DeleteIdentifier: delete the identifier name held in rd.
  DeleteIdentifier,
  /// GetBuiltin: rd = builtin[name]. Aux: name constant index (u16)
  GetBuiltin,

  // === Miscellaneous (2) ===
  /// InstanceOf: rd = rs1 instanceof rs2
  InstanceOf,
  /// Spread: call the callee in rd with values in rs expanded by the aux mask register.
  Spread,
  /// ResetLocalCell: create a fresh uninitialized lexical cell. Aux: slot index (u16)
  ResetLocalCell,
  /// Load a RISC spill slot into `rd`. Aux: spill index (u16).
  LoadSpill,
  /// Store `rd` into a RISC spill slot. Aux: spill index (u16).
  StoreSpill,
  /// rd = ~rs (bitwise NOT)
  BitNot,
  /// GetAsyncIterator: acquire an async iterator record for rd.
  GetAsyncIterator,
}

/// Total number of opcode variants.
pub const OP_COUNT: usize = Op::GetAsyncIterator as usize + 1;

impl Op {
  /// Converts a byte to an Op if valid, returns None otherwise
  pub fn from_byte(b: u8) -> Option<Op> {
    Op::try_from(b).ok()
  }

  /// Returns the byte representation of this opcode.
  pub fn to_byte(self) -> u8 {
    self as u8
  }

  /// Returns the human-readable name of this opcode.
  pub fn name(&self) -> &'static str {
    match self {
      Op::Const => "Const",
      Op::Null => "Null",
      Op::True => "True",
      Op::False => "False",
      Op::Undefined => "Undefined",
      Op::LoadLocal => "LoadLocal",
      Op::StoreLocal => "StoreLocal",
      Op::Move => "Move",
      Op::Add => "Add",
      Op::Sub => "Sub",
      Op::Mul => "Mul",
      Op::Div => "Div",
      Op::Mod => "Mod",
      Op::Pow => "Pow",
      Op::Neg => "Neg",
      Op::BitAnd => "BitAnd",
      Op::BitOr => "BitOr",
      Op::BitXor => "BitXor",
      Op::Shl => "Shl",
      Op::Shr => "Shr",
      Op::Ushr => "Ushr",
      Op::CompareJump => "CompareJump",
      Op::Jump => "Jump",
      Op::JumpIfTrue => "JumpIfTrue",
      Op::JumpIfFalse => "JumpIfFalse",
      Op::JumpIfNullish => "JumpIfNullish",
      Op::And => "And",
      Op::Or => "Or",
      Op::Not => "Not",
      Op::Typeof => "Typeof",
      Op::GetProperty => "GetProperty",
      Op::SetProperty => "SetProperty",
      Op::DeleteProperty => "DeleteProperty",
      Op::HasProperty => "HasProperty",
      Op::GetGlobal => "GetGlobal",
      Op::TypeofGlobal => "TypeofGlobal",
      Op::SetGlobal => "SetGlobal",
      Op::GetUpvalue => "GetUpvalue",
      Op::SetUpvalue => "SetUpvalue",
      Op::Call => "Call",
      Op::TailCall => "TailCall",
      Op::Construct => "Construct",
      Op::CallBuiltin => "CallBuiltin",
      Op::MakeClosure => "MakeClosure",
      Op::GetThis => "GetThis",
      Op::GetNewTarget => "GetNewTarget",
      Op::GetSuperProperty => "GetSuperProperty",
      Op::BuildArray => "BuildArray",
      Op::BuildObject => "BuildObject",
      Op::ArrayFromSpread => "ArrayFromSpread",
      Op::Throw => "Throw",
      Op::PushHandler => "PushHandler",
      Op::PopHandler => "PopHandler",
      Op::IteratorNext => "IteratorNext",
      Op::IteratorClose => "IteratorClose",
      Op::Yield => "Yield",
      Op::YieldStar => "YieldStar",
      Op::Return => "Return",
      Op::PushWithEnv => "PushWithEnv",
      Op::PopEnv => "PopEnv",
      Op::AwaitPromise => "AwaitPromise",
      Op::SuperConstruct => "SuperConstruct",
      Op::GeneratorInitDone => "GeneratorInitDone",
      Op::GetModuleExport => "GetModuleExport",
      Op::SetModuleExport => "SetModuleExport",
      Op::CloneLocalCell => "CloneLocalCell",
      Op::SetSuperProperty => "SetSuperProperty",
      Op::DeleteIdentifier => "DeleteIdentifier",
      Op::GetBuiltin => "GetBuiltin",
      Op::InstanceOf => "InstanceOf",
      Op::Spread => "Spread",
      Op::ResetLocalCell => "ResetLocalCell",
      Op::LoadSpill => "LoadSpill",
      Op::StoreSpill => "StoreSpill",
      Op::BitNot => "BitNot",
      Op::GetAsyncIterator => "GetAsyncIterator",
    }
  }

  /// Returns whether this opcode is a jump (conditional or unconditional).
  pub fn is_jump(&self) -> bool {
    matches!(
      self,
      Op::CompareJump | Op::Jump | Op::JumpIfTrue | Op::JumpIfFalse | Op::JumpIfNullish
    )
  }

  /// Returns whether this opcode is an arithmetic operation.
  pub fn is_arithmetic(&self) -> bool {
    matches!(
      self,
      Op::Add | Op::Sub | Op::Mul | Op::Div | Op::Mod | Op::Pow | Op::Neg
    )
  }

  /// Returns whether this opcode is a comparison operation.
  pub fn is_comparison(&self) -> bool {
    matches!(self, Op::CompareJump)
  }

  /// Returns whether this opcode is a logical or boolean operator.
  pub fn is_logical(&self) -> bool {
    matches!(self, Op::And | Op::Or | Op::Not)
  }
}

impl TryFrom<u8> for Op {
  type Error = ();

  /// Converts a u8 value into an Op variant. Returns an error if the value is out of range.
  fn try_from(value: u8) -> Result<Self, Self::Error> {
    use Op::*;
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
      42 => Ok(CallBuiltin),
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
  use crate::value::ObjectData;
  use std::cell::RefCell;
  use std::rc::Rc;

  #[test]
  fn compiled_chunks_are_sendable_between_threads() {
    fn assert_send<T: Send>() {}
    assert_send::<Chunk>();
  }

  #[test]
  fn runtime_owned_values_are_rejected_as_constants() {
    let object = Value::Object(Rc::new(RefCell::new(ObjectData::new())));
    assert_eq!(Constant::try_from(object), Err(ConstantConversionError));
  }

  #[test]
  fn constants_expose_compact_immediates_without_heap_allocation() {
    let number = Constant::Number(-0.0)
      .as_tagged_immediate()
      .and_then(TaggedValue::as_number);
    assert_eq!(number.map(f64::to_bits), Some((-0.0_f64).to_bits()));
    assert_eq!(
      Constant::Bool(true)
        .as_tagged_immediate()
        .and_then(TaggedValue::as_boolean),
      Some(true)
    );
    assert_eq!(
      Constant::Undefined
        .as_tagged_immediate()
        .map(TaggedValue::kind),
      Some(crate::value::tagged::TaggedValueKind::Undefined)
    );
    assert!(
      Constant::String("heap".into())
        .as_tagged_immediate()
        .is_none()
    );
  }

  #[test]
  fn op_round_trip() {
    assert_eq!(OP_COUNT, Op::GetAsyncIterator as usize + 1);
    for i in 0u8..=Op::GetAsyncIterator as u8 {
      let Ok(op) = Op::try_from(i) else {
        assert!((50..=53).contains(&i), "unexpected opcode gap at {i}");
        continue;
      };
      let b = u8::from(op);
      assert_eq!(b, i);
      let op2 = Op::from_byte(b).expect("from_byte");
      assert_eq!(op2, op);
    }
    for retired in 50..=53 {
      assert!(Op::from_byte(retired).is_none());
    }
  }

  #[test]
  fn chunks_have_a_fixed_risc_v2_version() {
    let chunk = Chunk::default();
    assert_eq!(chunk.version, BytecodeVersion::RiscV2);
    assert_eq!(chunk.version as u8, 2);
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
    invalid_jump.write_instr(Op::Jump as u32 | (10 << 8), 1);
    assert!(
      invalid_jump
        .verify()
        .unwrap_err()
        .contains("out-of-bounds instruction")
    );
  }

  #[test]
  fn verifier_rejects_invalid_risc_operands_and_metadata() {
    let mut invalid_global = Chunk::default();
    invalid_global
      .constants
      .push(Constant::Number(1.0));
    invalid_global.write_instr(encode_risc_aux16(Op::GetGlobal, 0, 0), 1);
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
    invalid_local.write_instr(encode_risc_aux16(Op::LoadLocal, 0, 1), 1);
    assert!(
      invalid_local
        .verify()
        .unwrap_err()
        .contains("local slot 1")
    );

    let mut invalid_spill = Chunk {
      risc_spill_count: 1,
      ..Chunk::default()
    };
    invalid_spill.write_instr(encode_risc_aux16(Op::LoadSpill, 0, 1), 1);
    assert!(
      invalid_spill
        .verify()
        .unwrap_err()
        .contains("spill slot 1")
    );

    let mut invalid_window = Chunk {
      risc_spill_count: 2,
      risc_value_spill_count: 1,
      ..Chunk::default()
    };
    invalid_window.write_instr(encode_risc_aux16(Op::BuildArray, 0, 2), 1);
    assert!(
      invalid_window
        .verify()
        .unwrap_err()
        .contains("operand window")
    );

    let mut invalid_finally = Chunk::default();
    invalid_finally.write_instr(Op::Undefined as u32, 1);
    invalid_finally
      .finally_handlers
      .push(FinallyHandler {
        push_ip: 0,
        finalizer_ip: 0,
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
  fn decoded_branch_offsets_follow_risc_layouts() {
    let compare_word = encode_compare_jump(2, 3, 4, -3).expect("compare jump");
    let instruction = DecodedInstruction {
      op: Op::CompareJump,
      payload: compare_word >> 8,
    };
    assert_eq!(instruction.branch_offset(), Some(-3));

    let conditional_word =
      encode_risc_cond_jump(Op::JumpIfFalse, 7, -42).expect("conditional jump");
    let instruction = DecodedInstruction {
      op: Op::JumpIfFalse,
      payload: conditional_word >> 8,
    };
    assert_eq!(instruction.branch_offset(), Some(-42));
  }

  #[test]
  fn risc_compare_jump_encoding_round_trips() {
    let word = encode_compare_jump(6, 3, 14, -4096).expect("encodable branch");
    assert_eq!(decode_compare_jump(word), (6, 3, 14, -4096));
    assert!(encode_compare_jump(0, 0, 0, 4096).is_none());
  }

  #[test]
  fn verifier_uses_risc_conditional_branch_layout() {
    let mut chunk = Chunk::default();
    chunk.write_instr(
      encode_risc_cond_jump(Op::JumpIfTrue, 15, 1).expect("conditional jump"),
      1,
    );
    chunk.write_instr(Op::Undefined as u32, 1);
    chunk.write_instr(Op::Return as u32, 1);
    assert!(chunk.verify().is_ok());

    chunk.code[0] = encode_risc_cond_jump(Op::JumpIfTrue, 15, 10).expect("conditional jump");
    assert!(
      chunk
        .verify()
        .unwrap_err()
        .contains("out-of-bounds instruction")
    );
  }
}
