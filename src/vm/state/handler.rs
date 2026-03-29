use crate::bytecode::Chunk;
use crate::value::Value;
use std::rc::Rc;

/// Exception handler record.
pub(crate) struct HandlerRecord {
    pub(crate) chunk: Rc<Chunk>,
    pub(crate) handler_ip: usize,
    pub(crate) saved_stack_len: usize,
    pub(crate) saved_locals_len: usize,
    pub(crate) frame_count: usize,
}

pub(crate) enum StepOutcome {
    Continue,
    Return(Value),
    Yield(Value),
}
