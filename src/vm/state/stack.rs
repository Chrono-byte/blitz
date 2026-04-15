use super::frame::Frame;
use super::handler::HandlerRecord;
use crate::value::{PropertyKey, Value};

/// Manages VM call frames as a compact stack component.
#[derive(Default)]
pub(crate) struct CallStack {
    frames: Vec<Frame>,
}

impl CallStack {
    /// Creates a new call stack with a specified capacity.
    pub(crate) fn with_capacity(cap: usize) -> Self {
        Self {
            frames: Vec::with_capacity(cap),
        }
    }

    /// Returns the number of frames on the stack.
    pub(crate) fn len(&self) -> usize {
        self.frames.len()
    }

    /// Returns true if the stack is empty.
    pub(crate) fn is_empty(&self) -> bool {
        self.frames.is_empty()
    }

    /// Pushes a frame onto the stack.
    pub(crate) fn push(&mut self, frame: Frame) {
        self.frames.push(frame);
    }

    /// Pops and returns the last frame from the stack.
    pub(crate) fn pop(&mut self) -> Option<Frame> {
        self.frames.pop()
    }

    /// Returns a reference to the last frame without removing it.
    pub(crate) fn last(&self) -> Option<&Frame> {
        self.frames.last()
    }

    /// Truncates the stack to the specified length.
    pub(crate) fn truncate(&mut self, len: usize) {
        self.frames.truncate(len);
    }

    /// Clears all frames from the stack.
    pub(crate) fn clear(&mut self) {
        self.frames.clear();
    }

    /// Returns an iterator over the frames in the stack.
    pub(crate) fn iter(&self) -> std::slice::Iter<'_, Frame> {
        self.frames.iter()
    }

    /// Returns a mutable iterator over the frames in the stack.
    pub(crate) fn iter_mut(&mut self) -> std::slice::IterMut<'_, Frame> {
        self.frames.iter_mut()
    }
}

/// Thin handler stack wrapper that isolates exception handler bookkeeping.
#[derive(Default)]
pub(crate) struct HandlerStack {
    records: Vec<HandlerRecord>,
}

impl HandlerStack {
    /// Creates a new handler stack with a specified capacity.
    pub(crate) fn with_capacity(cap: usize) -> Self {
        Self {
            records: Vec::with_capacity(cap),
        }
    }

    /// Returns the number of handler records on the stack.
    pub(crate) fn len(&self) -> usize {
        self.records.len()
    }

    /// Pushes a handler record onto the stack.
    pub(crate) fn push(&mut self, record: HandlerRecord) {
        self.records.push(record);
    }

    /// Pops and returns the last handler record from the stack.
    pub(crate) fn pop(&mut self) -> Option<HandlerRecord> {
        self.records.pop()
    }

    /// Returns a reference to the last handler record without removing it.
    pub(crate) fn last(&self) -> Option<&HandlerRecord> {
        self.records.last()
    }

    /// Truncates the stack to the specified length.
    pub(crate) fn truncate(&mut self, len: usize) {
        self.records.truncate(len);
    }

    /// Clears all handler records from the stack.
    pub(crate) fn clear(&mut self) {
        self.records.clear();
    }
}

/// ECMAScript reference record (base, referenced name, strict mode flag).
#[derive(Clone)]
pub(crate) struct ReferenceRecord {
    /// The base object or environment for the reference.
    pub(crate) base: Value,
    /// The name or key being referenced.
    pub(crate) referenced_name: PropertyKey,
    /// Whether the reference is in strict mode.
    pub(crate) strict: bool,
}

impl ReferenceRecord {
    /// Creates a new reference record with the given base, name, and strict mode flag.
    pub(crate) fn new(base: Value, referenced_name: PropertyKey, strict: bool) -> Self {
        Self {
            base,
            referenced_name,
            strict,
        }
    }
}

/// Stack storing pending reference records for future assignments.
#[derive(Default)]
pub(crate) struct ReferenceStack {
    inner: Vec<ReferenceRecord>,
}

impl ReferenceStack {
    /// Creates a new empty reference stack.
    pub(crate) fn new() -> Self {
        Self { inner: Vec::new() }
    }

    /// Pushes a reference record onto the stack.
    pub(crate) fn push(&mut self, record: ReferenceRecord) {
        self.inner.push(record);
    }

    /// Pops and returns the last reference record from the stack.
    pub(crate) fn pop(&mut self) -> Option<ReferenceRecord> {
        self.inner.pop()
    }

    /// Returns a reference to the last reference record without removing it.
    pub(crate) fn last(&self) -> Option<&ReferenceRecord> {
        self.inner.last()
    }
}
