// SPDX-License-Identifier: MPL-2.0
//! Recomputed semantic facts used by lowering and search selection.

use crate::ir::IrNode;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct Analysis {
  pub(crate) nullable: bool,
  pub(crate) minimum_width: usize,
  pub(crate) capture_free: bool,
  pub(crate) nfa_eligible: bool,
}

impl Analysis {
  pub(crate) fn for_ir(node: &IrNode) -> Self {
    let (nullable, minimum_width) = properties(node);
    Self {
      nullable,
      minimum_width,
      capture_free: true,
      nfa_eligible: true,
    }
  }
}

fn properties(node: &IrNode) -> (bool, usize) {
  match node {
    IrNode::Empty | IrNode::Assert(_) => (true, 0),
    IrNode::Literal(_) | IrNode::Dot | IrNode::Class(_) => (false, 1),
    IrNode::Sequence(nodes) => nodes
      .iter()
      .fold((true, 0_usize), |result, node| {
        let next = properties(node);
        (result.0 && next.0, result.1.saturating_add(next.1))
      }),
    IrNode::Repeat { body, min, .. } => {
      let (body_nullable, body_width) = properties(body);
      (*min == 0 || body_nullable, body_width.saturating_mul(*min))
    }
  }
}
