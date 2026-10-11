// SPDX-License-Identifier: MPL-2.0

use crate::ir::{CaptureId, IrAssertion, IrNode};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct Analysis {
  pub(crate) nullable: bool,
  pub(crate) minimum_width: usize,
  pub(crate) capture_free: bool,
  pub(crate) nfa_eligible: bool,
  pub(crate) has_assertion: bool,
  pub(crate) has_backreference: bool,
}

impl Analysis {
  pub(crate) fn for_ir(node: &IrNode) -> Self {
    let facts = properties(node);
    Self {
      nullable: facts.nullable,
      minimum_width: facts.minimum_width,
      capture_free: !facts.has_capture,
      nfa_eligible: !facts.has_capture && !facts.has_assertion && !facts.has_backreference,
      has_assertion: facts.has_assertion,
      has_backreference: facts.has_backreference,
    }
  }
}

#[derive(Clone, Copy, Default)]
struct Facts {
  nullable: bool,
  minimum_width: usize,
  has_capture: bool,
  has_assertion: bool,
  has_backreference: bool,
}

fn properties(node: &IrNode) -> Facts {
  match node {
    IrNode::Empty => Facts {
      nullable: true,
      ..Facts::default()
    },
    IrNode::Assert(assertion) => {
      let nested = match assertion {
        IrAssertion::Lookahead { body, .. } => properties(body),
        IrAssertion::Start | IrAssertion::End | IrAssertion::WordBoundary(_) => Facts::default(),
      };
      Facts {
        nullable: true,
        has_assertion: true,
        ..nested
      }
    }
    IrNode::Backreference(_) => Facts {
      nullable: true,
      has_backreference: true,
      ..Facts::default()
    },
    IrNode::Literal(_) | IrNode::Dot | IrNode::Class(_) => Facts {
      minimum_width: 1,
      ..Facts::default()
    },
    IrNode::Sequence(nodes) => nodes.iter().fold(
      Facts {
        nullable: true,
        ..Facts::default()
      },
      combine_sequence,
    ),
    IrNode::Alternative(nodes) => alternative_facts(nodes),
    IrNode::Capture { body, .. } => {
      let mut result = properties(body);
      result.has_capture = true;
      result
    }
    IrNode::Repeat { body, min, .. } => {
      let mut result = properties(body);
      result.nullable = *min == 0 || result.nullable;
      result.minimum_width = result.minimum_width.saturating_mul(*min);
      result
    }
  }
}

fn combine_sequence(
  mut result: Facts,
  node: &IrNode,
) -> Facts {
  let next = properties(node);
  result.nullable &= next.nullable;
  result.minimum_width = result
    .minimum_width
    .saturating_add(next.minimum_width);
  merge_flags(&mut result, next);
  result
}

fn alternative_facts(nodes: &[IrNode]) -> Facts {
  let mut result = Facts {
    minimum_width: usize::MAX,
    ..Facts::default()
  };
  for node in nodes {
    let next = properties(node);
    result.nullable |= next.nullable;
    result.minimum_width = result.minimum_width.min(next.minimum_width);
    merge_flags(&mut result, next);
  }
  if nodes.is_empty() {
    result.minimum_width = 0;
  }
  result
}

fn merge_flags(
  result: &mut Facts,
  next: Facts,
) {
  result.has_capture |= next.has_capture;
  result.has_assertion |= next.has_assertion;
  result.has_backreference |= next.has_backreference;
}

pub(crate) fn captures_written(node: &IrNode) -> Box<[CaptureId]> {
  let mut captures = Vec::new();
  collect_captures(node, &mut captures);
  captures.sort_unstable_by_key(|capture| capture.index());
  captures.dedup();
  captures.into_boxed_slice()
}

fn collect_captures(
  node: &IrNode,
  captures: &mut Vec<CaptureId>,
) {
  match node {
    IrNode::Capture { capture, body } => {
      captures.push(*capture);
      collect_captures(body, captures);
    }
    IrNode::Sequence(nodes) | IrNode::Alternative(nodes) => {
      for node in nodes {
        collect_captures(node, captures);
      }
    }
    IrNode::Repeat { body, .. } | IrNode::Assert(IrAssertion::Lookahead { body, .. }) => {
      collect_captures(body, captures);
    }
    IrNode::Empty
    | IrNode::Literal(_)
    | IrNode::Dot
    | IrNode::Class(_)
    | IrNode::Assert(_)
    | IrNode::Backreference(_) => {}
  }
}
