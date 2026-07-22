//! Host-neutral WebSocket lifecycle state used by runtime adapters.

/// WebSocket connection state, matching the Web platform numeric constants.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum WebSocketReadyState {
  /// The opening handshake has not completed.
  Connecting = 0,
  /// The connection can send and receive messages.
  Open = 1,
  /// A close handshake has started.
  Closing = 2,
  /// The connection is no longer usable.
  Closed = 3,
}

/// VM-owned state shared by host WebSocket object implementations.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WebSocketState {
  ready_state: WebSocketReadyState,
  buffered_amount: usize,
}

impl Default for WebSocketState {
  fn default() -> Self {
    Self {
      ready_state: WebSocketReadyState::Connecting,
      buffered_amount: 0,
    }
  }
}

impl WebSocketState {
  /// Return the current ready state.
  #[must_use]
  pub const fn ready_state(&self) -> WebSocketReadyState {
    self.ready_state
  }

  /// Return bytes queued for transmission.
  #[must_use]
  pub const fn buffered_amount(&self) -> usize {
    self.buffered_amount
  }

  /// Complete the opening transition.
  ///
  /// Returns `false` when the connection has already moved past connecting.
  pub fn open(&mut self) -> bool {
    if self.ready_state != WebSocketReadyState::Connecting {
      return false;
    }
    self.ready_state = WebSocketReadyState::Open;
    true
  }

  /// Begin closing an open or connecting connection.
  pub fn begin_close(&mut self) {
    if self.ready_state != WebSocketReadyState::Closed {
      self.ready_state = WebSocketReadyState::Closing;
    }
  }

  /// Finish closing and discard accounting for unsent bytes.
  pub fn finish_close(&mut self) {
    self.ready_state = WebSocketReadyState::Closed;
    self.buffered_amount = 0;
  }

  /// Account for bytes accepted by the host's outbound queue.
  ///
  /// Returns `false` unless the connection is open or the counter would
  /// overflow.
  pub fn queue_bytes(
    &mut self,
    amount: usize,
  ) -> bool {
    if self.ready_state != WebSocketReadyState::Open {
      return false;
    }
    let Some(total) = self.buffered_amount.checked_add(amount) else {
      return false;
    };
    self.buffered_amount = total;
    true
  }

  /// Account for bytes flushed by the host transport.
  pub fn drain_bytes(
    &mut self,
    amount: usize,
  ) {
    self.buffered_amount = self.buffered_amount.saturating_sub(amount);
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn tracks_lifecycle_and_buffered_bytes() {
    let mut state = WebSocketState::default();
    assert_eq!(state.ready_state(), WebSocketReadyState::Connecting);
    assert!(!state.queue_bytes(4));
    assert!(state.open());
    assert!(state.queue_bytes(7));
    state.drain_bytes(2);
    assert_eq!(state.buffered_amount(), 5);
    state.begin_close();
    assert_eq!(state.ready_state(), WebSocketReadyState::Closing);
    assert!(!state.queue_bytes(1));
    state.finish_close();
    assert_eq!(state.ready_state(), WebSocketReadyState::Closed);
    assert_eq!(state.buffered_amount(), 0);
  }
}
