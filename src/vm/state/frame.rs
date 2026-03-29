use crate::bytecode::Chunk;
use crate::environment::EnvironmentRecord;
use crate::typecheck::Type;
use crate::value::Value;
use std::cell::RefCell;
use std::collections::VecDeque;
use std::rc::Rc;

/// Call frame.
pub(crate) struct Frame {
    pub(crate) chunk: Rc<Chunk>,
    pub(crate) ip: usize,
    pub(crate) stack: Vec<Value>,
    pub(crate) locals: Vec<Rc<RefCell<Value>>>,
    pub(crate) this_value: Value,
    pub(crate) closure_env: Option<Rc<RefCell<Vec<Rc<RefCell<Value>>>>>>,
    pub(crate) slot_types: Option<Vec<Option<Type>>>,
    /// Length of caller stack at call entry (diagnostic only).
    pub(crate) stack_base: usize,
    /// Whether this frame executes in strict mode.
    /// Set from the callee's FunctionProto::is_strict field when the frame
    /// is created during function entry. Used at runtime to apply strict
    /// mode semantics (assignment errors, this binding, etc.).
    pub(crate) is_strict: bool,
    pub(crate) lexical_env: Rc<RefCell<EnvironmentRecord>>,
    pub(crate) yield_star_queue: Option<VecDeque<Value>>,
}
