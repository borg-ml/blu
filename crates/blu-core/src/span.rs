use crate::SourceId;
use core::fmt;

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
#[repr(transparent)]
pub struct ByteOffset(u32);

impl ByteOffset {
    #[must_use]
    pub const fn new(value: u32) -> Self {
        Self(value)
    }

    pub fn from_usize(value: usize) -> Result<Self, SpanError> {
        u32::try_from(value)
            .map(Self)
            .map_err(|_| SpanError::OffsetOverflow { value })
    }

    #[must_use]
    pub const fn get(self) -> u32 {
        self.0
    }

    #[must_use]
    pub const fn as_usize(self) -> usize {
        self.0 as usize
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
/// A validated half-open byte range `[start, end)` within one source.
pub struct ByteSpan {
    source: SourceId,
    start: ByteOffset,
    end: ByteOffset,
}

impl ByteSpan {
    pub fn new(source: SourceId, start: ByteOffset, end: ByteOffset) -> Result<Self, SpanError> {
        if start > end {
            Err(SpanError::Reversed { start, end })
        } else {
            Ok(Self { source, start, end })
        }
    }

    pub fn from_usize(source: SourceId, start: usize, end: usize) -> Result<Self, SpanError> {
        Self::new(
            source,
            ByteOffset::from_usize(start)?,
            ByteOffset::from_usize(end)?,
        )
    }

    #[must_use]
    pub const fn source(self) -> SourceId {
        self.source
    }

    #[must_use]
    pub const fn start(self) -> ByteOffset {
        self.start
    }

    #[must_use]
    pub const fn end(self) -> ByteOffset {
        self.end
    }

    #[must_use]
    pub const fn len(self) -> u32 {
        self.end.0 - self.start.0
    }

    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.start.0 == self.end.0
    }

    #[must_use]
    pub const fn contains(self, offset: ByteOffset) -> bool {
        self.start.0 <= offset.0 && offset.0 < self.end.0
    }

    pub fn merge(self, other: Self) -> Result<Self, SpanError> {
        if self.source != other.source {
            return Err(SpanError::SourceMismatch {
                expected: self.source,
                actual: other.source,
            });
        }
        Self::new(
            self.source,
            self.start.min(other.start),
            self.end.max(other.end),
        )
    }

    pub(crate) fn validate_for(
        self,
        source: SourceId,
        source_len: ByteOffset,
    ) -> Result<(), SpanError> {
        if self.source != source {
            return Err(SpanError::SourceMismatch {
                expected: source,
                actual: self.source,
            });
        }
        if self.end > source_len {
            return Err(SpanError::OutOfBounds {
                end: self.end,
                source_len,
            });
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SpanError {
    OffsetOverflow {
        value: usize,
    },
    Reversed {
        start: ByteOffset,
        end: ByteOffset,
    },
    SourceMismatch {
        expected: SourceId,
        actual: SourceId,
    },
    OutOfBounds {
        end: ByteOffset,
        source_len: ByteOffset,
    },
}

impl fmt::Display for SpanError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::OffsetOverflow { value } => {
                write!(formatter, "byte offset {value} exceeds u32 range")
            }
            Self::Reversed { start, end } => write!(
                formatter,
                "byte span starts at {} after ending at {}",
                start.get(),
                end.get()
            ),
            Self::SourceMismatch { expected, actual } => {
                write!(formatter, "expected {expected}, found span for {actual}")
            }
            Self::OutOfBounds { end, source_len } => write!(
                formatter,
                "byte span ends at {}, beyond source length {}",
                end.get(),
                source_len.get()
            ),
        }
    }
}

impl std::error::Error for SpanError {}
