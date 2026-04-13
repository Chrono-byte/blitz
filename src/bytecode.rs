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
//! - CompareJump: cond(3 bits) | rs1(4) | rs2(4) | offset_i13(15 bits) packed in bits 8-31
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
//! chunk.write(Op::Const as u8, 1);
//! chunk.write(0, 1); // Constant index
//! ```

use crate::typecheck::Type;
use crate::value::Value;
use oxc_span::Span;
use std::convert::TryFrom;
use std::mem;
use std::rc::Rc;

#[derive(Clone, Debug)]
/// Description of a captured binding for a closure.
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

#[derive(Clone, Debug)]
/// Unified inline cache entry used for property access, global access, and calls.
/// All IC lookups in RISC bytecode share this format.
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

#[derive(Clone, Debug)]
/// A chunk of bytecode containing opcodes, constants, and metadata
pub struct Chunk {
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
    /// Unified inline cache table for this function (RISC bytecode).
    /// Indexed by 4-bit IC slot field in bytecode (0-15 slots max).
    pub ic_table: Vec<ICEntry>,
}

impl Chunk {
    /// Creates a new empty chunk
    pub fn new(source_text: Rc<String>) -> Self {
        Self {
            code: Vec::new(),
            constants: Vec::new(),
            functions: Vec::new(),
            lines: Vec::new(),
            source_text,
            root_slot_types: Vec::new(),
            root_slot_names: Vec::new(),
            root_lexical_tdz: Vec::new(),
            root_strict: false,
            allow_new_target: false,
            allow_super: false,
        }
    }

    /// Writes a packed instruction word to the chunk with line number info.
    /// The instruction layout: low 8 bits = opcode, upper 24 bits = payload.
    pub fn write_instr(&mut self, instr: u32, line: u32) {
        self.code.push(instr);
        self.lines.push(line);
    }

    /// Compatibility helper: write a single opcode byte as an instruction word
    /// with empty payload. This mirrors the old `write(opcode, line)` helper
    /// used during the migration and keeps callsites simple.
    pub fn write(&mut self, opcode: u8, line: u32) {
        let instr = opcode as u32;
        self.write_instr(instr, line);
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

    /// Read the 24-bit signed payload (bits 8..31) from instruction at `idx`.
    /// Returns None if out of bounds.
    pub fn read_i24_payload_at(&self, idx: usize) -> Option<i32> {
        let w = *self.code.get(idx)?;
        // interpret upper 24 bits as signed i32 with sign extension
        let payload = (w >> 8) & 0x00FF_FFFF;
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
    pub fn write_i24_payload_at(&mut self, idx: usize, val: i32) -> bool {
        if idx >= self.code.len() {
            return false;
        }
        // ensure fits in signed 24-bit
        if val < -(1 << 23) || val >= (1 << 23) {
            return false;
        }
        let instr = self.code[idx];
        // clear upper 24 bits then or in new payload
        let payload_u32 = ((val as u32) & 0x00FF_FFFF) << 8;
        let new = (instr & 0x0000_00FF) | payload_u32;
        self.code[idx] = new;
        true
    }

    /// Compatibility helper: write a signed 16-bit payload into the
    /// instruction at `idx`. During the fixed-width migration most callers
    /// now expect to patch a 24-bit payload; however some remaining code still
    /// emits 16-bit patches. Implement this by sign-extending the i16 into
    /// the 24-bit payload field.
    pub fn write_i16_at(&mut self, idx: usize, val: i16) -> bool {
        self.write_i24_payload_at(idx, val as i32)
    }

    /// Helper: returns the instruction index of the jump target for a jump located at `ip`.
    /// `ip` is the index of the opcode byte. Returns `None` if `ip` is out of bounds,
    /// the opcode at `ip` is not a jump, or the computed target is out of bounds.
    /// Given an instruction-index `ip`, return the instruction index of the
    /// jump target if the instruction at `ip` is a jump. In the new fixed
    /// width layout the next instruction is always `ip + 1` and the signed
    /// 24-bit payload is the relative offset.
    pub fn jump_target_from_ip(&self, ip: usize) -> Option<usize> {
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
        Some(target as usize)
    }

    /// Hotwire the jump at `ip` so it jumps to the following instruction (i.e. becomes a no-op jump).
    /// Returns the original `Op` if successful, or `None` on error.
    pub fn hotwire_jump_to_next(&mut self, ip: usize) -> Option<Op> {
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
    pub fn hotwire_make_unconditional(&mut self, ip: usize) -> Option<Op> {
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
    pub fn hotwire_redirect(&mut self, ip: usize, target_ip: usize) -> Option<Op> {
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
        // ensure fits in signed 24-bit
        if offset < -(1 << 23) || offset >= (1 << 23) {
            return None;
        }
        if self.write_i24_payload_at(ip, offset as i32) {
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
pub fn encode_registers(rd: u8, rs: u8) -> u32 {
    encode_rd(rd) | encode_rs(rs)
}

// ============================================================================

/// Returns the opcode from `instr`.
#[inline]
pub fn opcode_from_instr(instr: u32) -> u8 {
    (instr & 0xFF) as u8
}

/// Returns the `idx`th payload byte from `instr`, where `idx` is 0-based.
#[inline]
pub fn payload_byte(instr: u32, idx: usize) -> u8 {
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

    // === Miscellaneous (2) ===
    /// InstanceOf: rd = rs1 instanceof rs2
    InstanceOf,
    /// Spread: spread array into arguments
    Spread,
}

/// Total number of opcode variants.
pub const OP_COUNT: usize = Op::Spread as usize + 1;

impl Op {
    /// Converts a byte to an Op if valid, returns None otherwise
    pub fn from_byte(b: u8) -> Option<Op> {
        Op::try_from(b).ok()
    }

    /// Converts an Op to a byte
    pub fn to_byte(&self) -> u8 {
        u8::from(*self)
    }

    /// Returns a stable display name for the opcode.
    pub fn name(self) -> &'static str {
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
            Op::InstanceOf => "InstanceOf",
            Op::Spread => "Spread",
        }
    }

    /// Returns a static slice of jump opcodes (no allocation).
    pub fn jumps() -> &'static [Op] {
        static JUMPS: [Op; 4] = [Op::Jump, Op::JumpIfTrue, Op::JumpIfFalse, Op::JumpIfNullish];
        &JUMPS
    }

    /// Returns the offset from the Opcode to the jump operand.
    /// This keeps the jump logic clean if you add more instructions!
    #[allow(dead_code)]
    fn jump_operand_offset(&self) -> usize {
        match self {
            Op::CompareJump => 1, // Special compound instruction
            _ => 1,
        }
    }

    /// Returns whether this opcode is a jump (conditional or unconditional).
    pub fn is_jump(&self) -> bool {
        matches!(
            self,
            Op::Jump | Op::JumpIfTrue | Op::JumpIfFalse | Op::JumpIfNullish | Op::CompareJump
        )
    }

    /// Returns whether this opcode performs an arithmetic operation that
    /// expects two operands.
    pub fn is_arithmetic(&self) -> bool {
        matches!(
            self,
            Op::Add | Op::Sub | Op::Mul | Op::Pow | Op::Div | Op::Mod
        )
    }

    /// Returns whether this opcode performs a comparison operation.
    pub fn is_comparison(&self) -> bool {
        // In RISC format, comparisons are fused with jumps via CompareJump
        // No standalone comparison opcodes
        false
    }

    /// Returns whether this opcode is a logical or boolean operator.
    pub fn is_logical(&self) -> bool {
        matches!(self, Op::And | Op::Or | Op::Not)
    }

    /// Returns whether this opcode manipulates the value stack directly
    /// without invoking arithmetic or logical semantics.
    pub fn is_stack_operation(&self) -> bool {
        matches!(self, Op::Pop | Op::Dup | Op::Swap | Op::PopN)
    }
}

// Safe conversions between `u8` and `Op`.
impl TryFrom<u8> for Op {
    type Error = ();

    fn try_from(b: u8) -> Result<Self, Self::Error> {
        // Compare as usize to avoid truncation when OP_COUNT doesn't fit in u8
        if (b as usize) < OP_COUNT {
            // Safety: `Op` is #[repr(u8)] and has contiguous discriminants from 0..=CompareJumpIfFalse.
            Ok(unsafe { mem::transmute::<u8, Op>(b) })
        } else {
            Err(())
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
        for i in 0u8..=Op::PopEnv as u8 {
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
        let mut c = Chunk::new(Rc::new(String::new()));
        // reserve some code so indices are meaningful
        // instruction at 0: Jump -> target will be at instruction index 6 (for example)
        // Build u32 instruction words. Each u32: low 8 bits = opcode, next bytes are payload
        c.code = vec![
            (Op::Jump.to_byte() as u32) | ((0x03u32) << 8) | ((0x00u32) << 16),
            // filler instructions (single-byte invalid ops encoded similarly)
            0xFFu32,
            0xFFu32,
            0xFFu32,
            0x00u32,
        ];
        // verify jump_target_from_ip
        // debug print
        eprintln!("c.code words: {:?}", c.code);
        let target = c.jump_target_from_ip(0).expect("target");
        // compute expected under fixed-width words: next = ip + 1 -> 1 + 3 = 4
        assert_eq!(target, 4usize);
        // hotwire to next: sets offset = 0 => target becomes next_instr (3)
        let orig = c.hotwire_jump_to_next(0).expect("hotwire");
        assert_eq!(orig, Op::Jump);
        let t2 = c.jump_target_from_ip(0).expect("target2");
        assert_eq!(t2, 1usize);

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
