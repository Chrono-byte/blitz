// SPDX-License-Identifier: MPL-2.0
use crate::{Error, ErrorKind};
use std::fmt;

/// Parsed ECMAScript regular-expression flags.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Flags {
  bits: u16,
}

impl fmt::Display for Flags {
  fn fmt(
    &self,
    formatter: &mut fmt::Formatter<'_>,
  ) -> fmt::Result {
    for (enabled, flag) in [
      (self.has_indices(), 'd'),
      (self.global(), 'g'),
      (self.ignore_case(), 'i'),
      (self.multiline(), 'm'),
      (self.dot_all(), 's'),
      (self.unicode() && !self.unicode_sets(), 'u'),
      (self.unicode_sets(), 'v'),
      (self.sticky(), 'y'),
    ] {
      if enabled {
        write!(formatter, "{flag}")?;
      }
    }
    Ok(())
  }
}

impl Flags {
  const GLOBAL: u16 = 1 << 0;
  const IGNORE_CASE: u16 = 1 << 1;
  const MULTILINE: u16 = 1 << 2;
  const DOT_ALL: u16 = 1 << 3;
  const UNICODE: u16 = 1 << 4;
  const STICKY: u16 = 1 << 5;
  const HAS_INDICES: u16 = 1 << 6;
  const UNICODE_SETS: u16 = 1 << 7;

  /// Parse a JavaScript flag string.
  pub fn parse(flags: &str) -> Result<Self, Error> {
    let mut parsed = Self::default();
    for (offset, character) in flags.encode_utf16().enumerate() {
      let bit = match character {
        value if value == u16::from(b'g') => Self::GLOBAL,
        value if value == u16::from(b'i') => Self::IGNORE_CASE,
        value if value == u16::from(b'm') => Self::MULTILINE,
        value if value == u16::from(b's') => Self::DOT_ALL,
        value if value == u16::from(b'u') => Self::UNICODE,
        value if value == u16::from(b'y') => Self::STICKY,
        value if value == u16::from(b'd') => Self::HAS_INDICES,
        value if value == u16::from(b'v') => Self::UNICODE_SETS,
        _ => {
          return Err(Error::new(
            ErrorKind::InvalidFlag,
            offset,
            "invalid regular-expression flag",
          ));
        }
      };
      if parsed.bits & bit != 0 {
        return Err(Error::new(
          ErrorKind::DuplicateFlag,
          offset,
          "duplicate regular-expression flag",
        ));
      }
      parsed.bits |= bit;
      if parsed.bits & Self::UNICODE != 0 && parsed.bits & Self::UNICODE_SETS != 0 {
        return Err(Error::new(
          ErrorKind::InvalidFlag,
          offset,
          "regular expression cannot use both 'u' and 'v' flags",
        ));
      }
    }
    Ok(parsed)
  }

  /// Whether the global flag is set.
  #[must_use]
  pub const fn global(self) -> bool {
    self.bits & Self::GLOBAL != 0
  }

  /// Whether matching ignores case.
  #[must_use]
  pub const fn ignore_case(self) -> bool {
    self.bits & Self::IGNORE_CASE != 0
  }

  /// Whether line anchors operate in multiline mode.
  #[must_use]
  pub const fn multiline(self) -> bool {
    self.bits & Self::MULTILINE != 0
  }

  /// Whether dot matches line terminators.
  #[must_use]
  pub const fn dot_all(self) -> bool {
    self.bits & Self::DOT_ALL != 0
  }

  /// Whether Unicode code-point matching is enabled.
  #[must_use]
  pub const fn unicode(self) -> bool {
    self.bits & (Self::UNICODE | Self::UNICODE_SETS) != 0
  }

  /// Whether Unicode sets mode (`v`) is enabled.
  #[must_use]
  pub const fn unicode_sets(self) -> bool {
    self.bits & Self::UNICODE_SETS != 0
  }

  /// Whether matching is sticky.
  #[must_use]
  pub const fn sticky(self) -> bool {
    self.bits & Self::STICKY != 0
  }

  /// Whether match indices should be exposed by the host.
  #[must_use]
  pub const fn has_indices(self) -> bool {
    self.bits & Self::HAS_INDICES != 0
  }
}
