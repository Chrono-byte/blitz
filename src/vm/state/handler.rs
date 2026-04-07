use crate::bytecode::Chunk;
use crate::environment::EnvironmentRecord;
use crate::value::Value;
use std::cell::RefCell;
use std::rc::Rc;

/// Exception handler record.
pub(crate) struct HandlerRecord {
    pub(crate) chunk: Rc<Chunk>,
    pub(crate) handler_ip: usize,
    pub(crate) saved_stack_len: usize,
    pub(crate) saved_locals_len: usize,
    pub(crate) frame_count: usize,
    pub(crate) saved_lexical_env: Rc<RefCell<EnvironmentRecord>>,
}

pub(crate) enum StepOutcome {
    Continue,
    Return(Value),
    Yield(Value),
}
