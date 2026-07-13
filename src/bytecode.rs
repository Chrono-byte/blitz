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
//! use blitz::bytecode::{Chunk, Op};
//! use std::rc::Rc;
//!
//! let mut chunk = Chunk::new(Rc::new(String::new()));
//! chunk.write_instr(Op::Const as u32, 1); // constant index 0
//! ```

use crate::typecheck::Type;
use crate::value::Value;
use oxc_span::Span;
use std::convert::TryFrom;
use std::rc::Rc;

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

/// Unified inline cache entry used for property access, global access, and calls.
/// All IC lookups in RISC bytecode share this format.
#[derive(Clone, Debug)]
pub struct ICEntry {
  /// Cache validity key: generation ID + shape hash
  /// For properties: receiver.generation << 32 | shape_hash
  /// For globals: binding.generation
  /// For calls: receiver.generation << 32 | function_identity
  pub key: u64,

  /// Cached result (interpretation depends on IC type in metadata):
  /// - Property get: property offset (u32) in upper bits
  /// - Property set: descriptor flags (u32) in upper bits
  /// - Global: slot offset (u32) in upper bits
  /// - Call: cached function ptr or type info (u64)
  pub value: u64,

  /// Generation stamp for this IC entry (used for invalidation)
  pub generation: u32,

  /// IC type: 0=Property, 1=Global, 2=Call
  pub ic_type: u8,

  /// Reserved for future use (profiling, optimization hints)
  pub reserved: u8,
}

impl ICEntry {
  /// Creates a new invalid IC entry
  pub fn new_invalid() -> Self {
    Self {
      key: 0,
      value: 0,
      generation: 0,
      ic_type: 0,
      reserved: 0,
    }
  }
}

/// Bytecode execution format carried by every chunk.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum BytecodeFormat {
  /// Fixed-width encoding interpreted with the legacy operand-stack semantics.
  #[default]
  StackV1,
  /// Fixed-width encoding interpreted with the 16-register RISC v2 semantics.
  RiscV2,
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
      Op::CompareJump => Some(((self.payload >> 8) as u16 as i16) as i32),
      Op::Jump | Op::JumpIfFalse | Op::JumpIfTrue | Op::JumpIfNullish | Op::PushHandler => {
        Some(self.operand_i24())
      }
      _ => None,
    }
  }
}

/// A chunk of bytecode containing opcodes, constants, and metadata
#[derive(Clone, Debug)]
pub struct Chunk {
  /// Execution semantics for the encoded instruction words.
  pub format: BytecodeFormat,
  /// Instructions stored as fixed-width 32-bit words. Each u32 contains
  /// opcode in the low 8 bits and a 24-bit payload in the upper bits.
  pub code: Vec<u32>,
  /// Constant pool values
  pub constants: Vec<Value>,
  /// Function prototypes
  pub functions: Vec<FunctionProto>,
  /// Line number information for debugging
  pub lines: Vec<u32>,
  /// Original source text for this chunk
  pub source_text: Rc<String>,
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
  pub chunk: Rc<Chunk>,
  /// NOTE: slot type annotations have been removed in JS-first mode; the
  /// runtime no longer enforces TypeScript annotations.
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
  /// Unified inline cache table for this function.
  /// Stack v1 sites currently use an 8-bit site index. RISC v2 sites use
  /// side metadata so the register operand fields do not cap the site count.
  pub ic_table: Vec<ICEntry>,
}

impl Chunk {
  /// Creates a new empty chunk
  pub fn new(source_text: Rc<String>) -> Self {
    Self {
      format: BytecodeFormat::StackV1,
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
    }
  }

  /// Decode one instruction through the format-independent word decoder.
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
    let detail = match self.format {
      BytecodeFormat::RiscV2 if instruction.op == Op::CompareJump => {
        let (condition, left, right, offset) = decode_compare_jump(word);
        format!(
          "compare={} r{} r{} offset={}",
          condition, left, right, offset
        )
      }
      BytecodeFormat::RiscV2
        if matches!(
          instruction.op,
          Op::JumpIfFalse | Op::JumpIfTrue | Op::JumpIfNullish
        ) =>
      {
        format!(
          "r{} offset={}",
          decode_rd(word),
          decode_risc_cond_offset(word)
        )
      }
      BytecodeFormat::RiscV2 => format!(
        "r{} r{} aux={} aux2={}",
        decode_rd(word),
        decode_rs(word),
        decode_aux8(word),
        decode_aux8_2(word)
      ),
      BytecodeFormat::StackV1 => match instruction.op {
        Op::Const | Op::GetGlobal | Op::TypeofGlobal | Op::SetGlobal => {
          instruction.operand_u16().to_string()
        }
        Op::CompareJump => format!(
          "compare={} offset={}",
          instruction.operand_u8(0).unwrap_or_default(),
          instruction.branch_offset().unwrap_or_default()
        ),
        Op::Jump | Op::JumpIfFalse | Op::JumpIfTrue | Op::JumpIfNullish | Op::PushHandler => {
          format!("offset={}", instruction.branch_offset().unwrap_or_default())
        }
        _ if instruction.payload != 0 => instruction
          .operand_u8(0)
          .unwrap_or_default()
          .to_string(),
        _ => String::new(),
      },
    };
    if detail.is_empty() {
      Ok(format!("{:#06} {}", ip, instruction.op.name()))
    } else {
      Ok(format!("{:#06} {} {}", ip, instruction.op.name(), detail))
    }
  }

  /// Validate structural invariants before a chunk is executed.
  pub fn verify(&self) -> Result<(), String> {
    if self.lines.len() != self.code.len() {
      return Err(format!(
        "line table length {} does not match instruction count {}",
        self.lines.len(),
        self.code.len()
      ));
    }
    for ip in 0..self.code.len() {
      let instruction = self.decode_at(ip)?;
      let branch_offset = match self.format {
        BytecodeFormat::StackV1 => instruction.branch_offset(),
        BytecodeFormat::RiscV2 if instruction.op == Op::CompareJump => {
          Some(decode_compare_jump(self.code[ip]).3 as i32)
        }
        BytecodeFormat::RiscV2
          if matches!(
            instruction.op,
            Op::JumpIfFalse | Op::JumpIfTrue | Op::JumpIfNullish
          ) =>
        {
          Some(decode_risc_cond_offset(self.code[ip]))
        }
        BytecodeFormat::RiscV2 => instruction.branch_offset(),
      };
      if let Some(offset) = branch_offset {
        let target = ip as i64 + 1 + offset as i64;
        if !(0..self.code.len() as i64).contains(&target) {
          return Err(format!(
            "jump at instruction {} targets out-of-bounds instruction {}",
            ip, target
          ));
        }
      }
      let pool_index = match self.format {
        BytecodeFormat::StackV1 => instruction.operand_u16(),
        BytecodeFormat::RiscV2 => decode_risc_aux16(self.code[ip]),
      };
      match instruction.op {
        Op::Const if pool_index as usize >= self.constants.len() => {
          return Err(format!(
            "constant index {} at instruction {} is out of bounds",
            pool_index, ip
          ));
        }
        Op::MakeClosure if pool_index as usize >= self.functions.len() => {
          return Err(format!(
            "function index {} at instruction {} is out of bounds",
            pool_index, ip
          ));
        }
        _ => {}
      }
    }
    for function in &self.functions {
      function.chunk.verify()?;
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

  /// Compatibility helper: write a single opcode byte as an instruction word
  /// with empty payload. This mirrors the old `write(opcode, line)` helper
  /// used during the migration and keeps callsites simple.
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
    value: Value,
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

  /// Compatibility helper: write a signed 16-bit payload into the
  /// instruction at `idx`. During the fixed-width migration most callers
  /// now expect to patch a 24-bit payload; however some remaining code still
  /// emits 16-bit patches. Implement this by sign-extending the i16 into
  /// the 24-bit payload field.
  pub fn write_i16_at(
    &mut self,
    idx: usize,
    val: i16,
  ) -> bool {
    self.write_i24_payload_at(idx, val as i32)
  }

  /// Helper: returns the instruction index of the jump target for a jump located at `ip`.
  /// `ip` is the index of the opcode byte. Returns `None` if `ip` is out of bounds,
  /// the opcode at `ip` is not a jump, or the computed target is out of bounds.
  /// Given an instruction-index `ip`, return the instruction index of the
  /// jump target if the instruction at `ip` is a jump. In the new fixed
  /// width layout the next instruction is always `ip + 1` and the signed
  /// 24-bit payload is the relative offset.
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
    // read signed 24-bit payload from instruction at ip
    let offset = self.read_i24_payload_at(ip)?;
    // next instruction index is always ip + 1 (fixed-width u32 words)
    let next = ip as i64 + 1;
    let target = next.checked_add(offset as i64)?;
    if target < 0 || target >= (self.code.len() as i64) {
      return None;
    }
    // convert safely to usize after bounds check to avoid truncation on smaller platforms
    let target_usize = usize::try_from(target).ok()?;
    Some(target_usize)
  }

  /// Hotwire the jump at `ip` so it jumps to the following instruction (i.e. becomes a no-op jump).
  /// Returns the original `Op` if successful, or `None` on error.
  pub fn hotwire_jump_to_next(
    &mut self,
    ip: usize,
  ) -> Option<Op> {
    let instr = *self.code.get(ip)?;
    let op = Op::from_byte((instr & 0xFF) as u8)?;
    if !op.is_jump() {
      return None;
    }
    // set payload to 0
    if self.write_i24_payload_at(ip, 0) {
      Some(op)
    } else {
      None
    }
  }

  /// Convert `JumpIfFalse` at `ip` to unconditional `Jump`.
  /// Returns the original `Op` if successful, or `None` on error.
  pub fn hotwire_make_unconditional(
    &mut self,
    ip: usize,
  ) -> Option<Op> {
    let instr = *self.code.get(ip)?;
    let op = Op::from_byte((instr & 0xFF) as u8)?;
    match op {
      Op::JumpIfFalse => {
        // replace opcode byte
        let new = (instr & 0xFFFF_FF00) | (Op::Jump.to_byte() as u32);
        self.code[ip] = new;
        Some(op)
      }
      Op::Jump => Some(op),
      _ => None,
    }
  }

  /// Hotwire the jump at `ip` so it jumps to `target_ip`.
  /// Returns the original `Op` if successful, or `None` on error.
  pub fn hotwire_redirect(
    &mut self,
    ip: usize,
    target_ip: usize,
  ) -> Option<Op> {
    if target_ip >= self.code.len() {
      return None;
    }
    let instr = *self.code.get(ip)?;
    let op = Op::from_byte((instr & 0xFF) as u8)?;
    if !op.is_jump() {
      return None;
    }
    // next instruction is ip + 1
    let next = (ip as i64) + 1;
    let offset = (target_ip as i64).checked_sub(next)?;
    // convert to i32 only after verifying it fits in i32 to avoid truncation
    let offset_i32 = i32::try_from(offset).ok()?;
    // ensure fits in signed 24-bit
    if !(-(1 << 23_i32)..(1 << 23_i32)).contains(&offset) {
      return None;
    }
    if self.write_i24_payload_at(ip, offset_i32) {
      Some(op)
    } else {
      None
    }
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
    Self::new(Rc::new(String::new()))
  }
}

// ----------------------------------------------------------------------------
// Opcode definitions
// ----------------------------------------------------------------------------

#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq)]
/// Bytecode operation codes for RISC-inspired format
///
/// All opcodes fit in a single 32-bit word with this layout:
/// - Bits 0-7:    Opcode
/// - Bits 8-11:   rd register (destination)
/// - Bits 12-15:  rs register (source)
/// - Bits 16-31:  Auxiliary data (interpretation depends on opcode)
///
/// Special cases:
/// - Jump: aux field contains i24 signed offset (bits 8-31)
/// - CompareJump: cond(3) | rs1(4) | rs2(4) | offset_i13(15)
///
/// NOTE: The numeric discriminant of each `Op` variant is part of the runtime
/// bytecode format. Do not reorder or insert variants in the middle without
/// updating the bytecode version and migration logic.
pub enum Op {
  // === Constants & Immediate Values (5) ===
  /// Load constant from pool → rd. Aux: constant_index (u8)
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
  /// Load local slot → rd. Aux: slot_index (u8)
  LoadLocal,
  /// Store rd → local slot. Aux: slot_index (u8)
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
  /// CompareJump: cond_type(3bits) | rs1(4) | rs2(4) | offset_i13(15)
  /// Compares rs1 and rs2 according to cond_type, jumps if true.
  /// cond_type: 0=Eq, 1=Neq, 2=Lt, 3=Lte, 4=Gt, 5=Gte, 6=StrictEq, 7=StrictNeq
  CompareJump,
  /// Jump offset_i24 (unconditional)
  Jump,
  /// Jump offset_i24 if last comparison was true
  JumpIfTrue,
  /// Jump offset_i24 if last comparison was false
  JumpIfFalse,
  /// Jump offset_i24 if rs is null or undefined
  JumpIfNullish,

  // === Logical (4) ===
  /// rd = rd && rs (logical AND, short-circuits)
  And,
  /// rd = rd || rs (logical OR, short-circuits)
  Or,
  /// rd = !rs (logical NOT)
  Not,
  /// rd = typeof rs
  Typeof,

  // === Property Access with unified IC (4) ===
  /// rd = obj[key]. Aux: ic_slot (4 bits), unused (4 bits)
  GetProperty,
  /// obj[key] = value. Aux: ic_slot (4 bits), unused (4 bits)
  SetProperty,
  /// rd = delete obj[key]
  DeleteProperty,
  /// rd = (key in obj)
  HasProperty,

  // === Global Variable Access with unified IC (2) ===
  /// rd = GetGlobal(name_const_idx). Aux: name_idx (u8), ic_slot (4 bits)
  GetGlobal,
  /// rd = typeof name_const_idx. Aux: name_idx (u8), ic_slot (4 bits)
  TypeofGlobal,
  /// SetGlobal(name_const_idx, rd). Aux: name_idx (u8), ic_slot (4 bits)
  SetGlobal,

  // === Upvalue Access (2) ===
  /// rd = GetUpvalue(upval_idx). Aux: upval_idx (u8)
  GetUpvalue,
  /// SetUpvalue(upval_idx, rd). Aux: upval_idx (u8)
  SetUpvalue,

  // === Function Calls with unified IC (4) ===
  /// Call r0 with nargs in r1-r4 (+ stack if nargs > 3)
  /// Return value in r0. Aux: nargs (4 bits), ic_slot (4 bits)
  Call,
  /// TailCall: like Call but reuses current frame
  TailCall,
  /// Construct: new r0(...). Aux: nargs (4 bits)
  Construct,
  /// CallBuiltin: call built-in function. Aux: builtin_idx (u8)
  CallBuiltin,

  // === Closures & this/target (4) ===
  /// rd = MakeClosure(proto_idx). Aux: proto_idx (u8)
  MakeClosure,
  /// rd = GetThis
  GetThis,
  /// rd = GetNewTarget
  GetNewTarget,
  /// rd = GetSuperProperty(obj, prop_const_idx). Aux: prop_idx (u8)
  GetSuperProperty,

  // === Array & Object Construction (3) ===
  /// BuildArray: collect nelem values into array → r0. Aux: nelem (u8)
  BuildArray,
  /// BuildObject: collect npairs {k,v} into object → r0. Aux: npairs (u8)
  BuildObject,
  /// ArrayFromSpread: build array with spread
  ArrayFromSpread,

  // === Stack Management (4) ===
  /// Pop: discard top of stack
  Pop,
  /// PopN: pop count values from stack. Aux: count (u8)
  PopN,
  /// Dup: duplicate rd to stack
  Dup,
  /// Swap: swap top 2 stack values
  Swap,

  // === Exception Handling (3) ===
  /// Throw rd
  Throw,
  /// PushHandler: push exception handler. Aux: offset_i16 (bits 16-31)
  PushHandler,
  /// PopHandler: pop exception handler
  PopHandler,

  // === Iteration (4) ===
  /// IteratorNext: rd = iterator.next(). Aux: iterator_reg (4 bits)
  IteratorNext,
  /// IteratorClose: close iterator rs. Aux: iterator_reg (4 bits)
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
  /// AwaitPromise: await rs
  AwaitPromise,
  /// SuperConstruct: super(...) in derived constructor
  SuperConstruct,
  /// GeneratorInitDone: marker for end of generator prologue
  GeneratorInitDone,

  // === Module System (2) ===
  /// GetModuleExport: rd = modules[module_idx].exports[name_idx]
  GetModuleExport,
  /// SetModuleExport: modules[module_idx].exports[name_idx] = rd
  SetModuleExport,

  // === Semantic Operations (4) ===
  /// CloneLocalCell: clone a local cell for closure capture. Aux: slot_idx (u8)
  CloneLocalCell,
  /// SetSuperProperty: super.property = value
  SetSuperProperty,
  /// DeleteIdentifier: delete identifier reference. Aux: name_idx (u8)
  DeleteIdentifier,
  /// GetBuiltin: rd = builtin[name_idx]. Aux: name_idx (u8)
  GetBuiltin,

  // === Miscellaneous (2) ===
  /// InstanceOf: rd = rs1 instanceof rs2
  InstanceOf,
  /// Spread: spread array into arguments
  Spread,
  /// ResetLocalCell: create a fresh uninitialized lexical cell. Aux: slot_idx (u8)
  ResetLocalCell,
  /// Load a RISC spill slot into `rd`. Aux: spill index (u16).
  LoadSpill,
  /// Store `rd` into a RISC spill slot. Aux: spill index (u16).
  StoreSpill,
}

/// Total number of opcode variants.
pub const OP_COUNT: usize = Op::StoreSpill as usize + 1;

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
      Op::Pop => "Pop",
      Op::PopN => "PopN",
      Op::Dup => "Dup",
      Op::Swap => "Swap",
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

  /// Returns whether this opcode manipulates the value stack directly
  /// without invoking arithmetic or logical semantics.
  pub fn is_stack_operation(&self) -> bool {
    matches!(self, Op::Pop | Op::PopN | Op::Dup | Op::Swap)
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
      50 => Ok(Pop),
      51 => Ok(PopN),
      52 => Ok(Dup),
      53 => Ok(Swap),
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
    for i in 0u8..=Op::StoreSpill as u8 {
      let op = Op::try_from(i).expect("valid op");
      let b = u8::from(op);
      assert_eq!(b, i);
      let op2 = Op::from_byte(b).expect("from_byte");
      assert_eq!(op2, op);
    }
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
  fn compare_jump_decoder_uses_its_signed_i16_offset() {
    let payload = 2_u32 | ((-3_i16 as u16 as u32) << 8);
    let instruction = DecodedInstruction {
      op: Op::CompareJump,
      payload,
    };
    assert_eq!(instruction.branch_offset(), Some(-3));
  }

  #[test]
  fn risc_compare_jump_encoding_round_trips() {
    let word = encode_compare_jump(6, 3, 14, -4096).expect("encodable branch");
    assert_eq!(decode_compare_jump(word), (6, 3, 14, -4096));
    assert!(encode_compare_jump(0, 0, 0, 4096).is_none());
  }
}
