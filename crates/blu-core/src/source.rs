use crate::{
    ByteOffset, ByteSpan, IdentityError, IdentityLimits, SourceId, SourceIdentity, SpanError,
};
use core::fmt;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SourceLimits {
    pub max_bytes: usize,
    pub max_lines: usize,
    pub max_name_bytes: usize,
}

impl Default for SourceLimits {
    fn default() -> Self {
        Self {
            max_bytes: 64 * 1024 * 1024,
            max_lines: 1_000_000,
            max_name_bytes: 4 * 1024,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SourceLimit {
    Bytes,
    Lines,
    NameBytes,
}

impl fmt::Display for SourceLimit {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Bytes => formatter.write_str("source bytes"),
            Self::Lines => formatter.write_str("source lines"),
            Self::NameBytes => formatter.write_str("source name bytes"),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LinePosition {
    pub line: u32,
    pub byte_column: u32,
}

/// Index of zero-based physical lines. Both LF and CRLF start a new line after
/// the LF byte; columns are byte offsets and therefore do not require UTF-8.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LineIndex {
    starts: Vec<ByteOffset>,
    source_len: ByteOffset,
}

impl LineIndex {
    pub fn new(bytes: &[u8], max_lines: usize) -> Result<Self, LineIndexError> {
        let source_len =
            ByteOffset::from_usize(bytes.len()).map_err(|_| LineIndexError::OffsetOverflow {
                actual: bytes.len(),
                maximum: u32::MAX as usize,
            })?;
        if max_lines == 0 {
            return Err(LineIndexError::TooManyLines {
                actual: 1,
                limit: 0,
            });
        }

        let capacity = bytes
            .iter()
            .filter(|byte| **byte == b'\n')
            .count()
            .checked_add(1)
            .ok_or(LineIndexError::LineCountOverflow)?;
        if capacity > max_lines {
            return Err(LineIndexError::TooManyLines {
                actual: capacity,
                limit: max_lines,
            });
        }

        let mut starts = allocate_line_starts(capacity)?;
        starts.push(ByteOffset::new(0));
        for (index, byte) in bytes.iter().enumerate() {
            if *byte == b'\n' {
                let start = index.checked_add(1).ok_or(LineIndexError::OffsetOverflow {
                    actual: usize::MAX,
                    maximum: u32::MAX as usize,
                })?;
                starts.push(ByteOffset::from_usize(start).map_err(|_| {
                    LineIndexError::OffsetOverflow {
                        actual: start,
                        maximum: u32::MAX as usize,
                    }
                })?);
            }
        }
        Ok(Self { starts, source_len })
    }

    #[must_use]
    pub fn line_count(&self) -> usize {
        self.starts.len()
    }

    #[must_use]
    pub const fn source_len(&self) -> ByteOffset {
        self.source_len
    }

    pub fn line_start(&self, line: usize) -> Result<ByteOffset, LineIndexError> {
        self.starts
            .get(line)
            .copied()
            .ok_or(LineIndexError::LineOutOfBounds {
                line,
                line_count: self.starts.len(),
            })
    }

    pub fn position(&self, offset: ByteOffset) -> Result<LinePosition, LineIndexError> {
        if offset > self.source_len {
            return Err(LineIndexError::OffsetOutOfBounds {
                offset,
                source_len: self.source_len,
            });
        }
        let line = self.starts.partition_point(|start| *start <= offset) - 1;
        let line_u32 = u32::try_from(line).map_err(|_| LineIndexError::LineCountOverflow)?;
        Ok(LinePosition {
            line: line_u32,
            byte_column: offset.get() - self.starts[line].get(),
        })
    }

    pub fn line_span(&self, source: SourceId, line: usize) -> Result<ByteSpan, LineIndexError> {
        let start = self.line_start(line)?;
        let end = self
            .starts
            .get(line + 1)
            .copied()
            .unwrap_or(self.source_len);
        ByteSpan::new(source, start, end).map_err(LineIndexError::Span)
    }
}

fn allocate_line_starts(capacity: usize) -> Result<Vec<ByteOffset>, LineIndexError> {
    let mut starts = Vec::new();
    starts
        .try_reserve_exact(capacity)
        .map_err(|_| LineIndexError::AllocationFailed {
            requested_lines: capacity,
        })?;
    Ok(starts)
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SourceFile {
    identity: SourceIdentity,
    bytes: Vec<u8>,
    lines: LineIndex,
}

impl SourceFile {
    pub fn new(
        id: SourceId,
        name: impl Into<String>,
        bytes: impl Into<Vec<u8>>,
        limits: SourceLimits,
    ) -> Result<Self, SourceError> {
        let name = name.into();
        if name.len() > limits.max_name_bytes {
            return Err(SourceError::Limit {
                kind: SourceLimit::NameBytes,
                actual: name.len(),
                limit: limits.max_name_bytes,
            });
        }
        let identity_limits = IdentityLimits {
            max_source_name_bytes: limits.max_name_bytes,
            ..IdentityLimits::default()
        };
        let identity =
            SourceIdentity::new(id, name, identity_limits).map_err(SourceError::Identity)?;

        let bytes = bytes.into();
        if bytes.len() > limits.max_bytes {
            return Err(SourceError::Limit {
                kind: SourceLimit::Bytes,
                actual: bytes.len(),
                limit: limits.max_bytes,
            });
        }
        let lines = LineIndex::new(&bytes, limits.max_lines).map_err(|error| match error {
            LineIndexError::TooManyLines { actual, limit } => SourceError::Limit {
                kind: SourceLimit::Lines,
                actual,
                limit,
            },
            other => SourceError::LineIndex(other),
        })?;
        Ok(Self {
            identity,
            bytes,
            lines,
        })
    }

    #[must_use]
    pub const fn identity(&self) -> &SourceIdentity {
        &self.identity
    }

    #[must_use]
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    #[must_use]
    pub fn line_index(&self) -> &LineIndex {
        &self.lines
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.bytes.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.bytes.is_empty()
    }

    pub fn span(&self, start: usize, end: usize) -> Result<ByteSpan, SpanError> {
        let span = ByteSpan::from_usize(self.identity.id(), start, end)?;
        span.validate_for(self.identity.id(), self.lines.source_len())?;
        Ok(span)
    }

    pub fn slice(&self, span: ByteSpan) -> Result<&[u8], SpanError> {
        span.validate_for(self.identity.id(), self.lines.source_len())?;
        Ok(&self.bytes[span.start().as_usize()..span.end().as_usize()])
    }

    pub fn position(&self, offset: usize) -> Result<LinePosition, SourceError> {
        let offset = ByteOffset::from_usize(offset).map_err(SourceError::Span)?;
        self.lines.position(offset).map_err(SourceError::LineIndex)
    }

    pub fn line_content_span(&self, line: usize) -> Result<ByteSpan, SourceError> {
        let full = self
            .lines
            .line_span(self.identity.id(), line)
            .map_err(SourceError::LineIndex)?;
        let mut end = full.end().as_usize();
        if end > full.start().as_usize() && self.bytes[end - 1] == b'\n' {
            end -= 1;
            if end > full.start().as_usize() && self.bytes[end - 1] == b'\r' {
                end -= 1;
            }
        }
        ByteSpan::from_usize(self.identity.id(), full.start().as_usize(), end)
            .map_err(SourceError::Span)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum LineIndexError {
    AllocationFailed {
        requested_lines: usize,
    },
    TooManyLines {
        actual: usize,
        limit: usize,
    },
    LineCountOverflow,
    OffsetOverflow {
        actual: usize,
        maximum: usize,
    },
    OffsetOutOfBounds {
        offset: ByteOffset,
        source_len: ByteOffset,
    },
    LineOutOfBounds {
        line: usize,
        line_count: usize,
    },
    Span(SpanError),
}

impl fmt::Display for LineIndexError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::AllocationFailed { requested_lines } => write!(
                formatter,
                "failed to allocate a line index for {requested_lines} lines"
            ),
            Self::TooManyLines { actual, limit } => {
                write!(
                    formatter,
                    "source has {actual} lines, exceeding limit {limit}"
                )
            }
            Self::LineCountOverflow => formatter.write_str("source line count overflowed"),
            Self::OffsetOverflow { actual, maximum } => write!(
                formatter,
                "source byte length {actual} exceeds offset maximum {maximum}"
            ),
            Self::OffsetOutOfBounds { offset, source_len } => write!(
                formatter,
                "byte offset {} exceeds source length {}",
                offset.get(),
                source_len.get()
            ),
            Self::LineOutOfBounds { line, line_count } => {
                write!(formatter, "line {line} exceeds line count {line_count}")
            }
            Self::Span(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for LineIndexError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Span(error) => Some(error),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SourceError {
    Identity(IdentityError),
    Limit {
        kind: SourceLimit,
        actual: usize,
        limit: usize,
    },
    LineIndex(LineIndexError),
    Span(SpanError),
}

impl fmt::Display for SourceError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Identity(error) => error.fmt(formatter),
            Self::Limit {
                kind,
                actual,
                limit,
            } => write!(
                formatter,
                "{kind} count/size {actual} exceeds limit {limit}"
            ),
            Self::LineIndex(error) => error.fmt(formatter),
            Self::Span(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for SourceError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Identity(error) => Some(error),
            Self::LineIndex(error) => Some(error),
            Self::Span(error) => Some(error),
            Self::Limit { .. } => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{LineIndexError, allocate_line_starts};

    #[test]
    fn line_index_capacity_failure_is_structured() {
        assert_eq!(
            allocate_line_starts(usize::MAX),
            Err(LineIndexError::AllocationFailed {
                requested_lines: usize::MAX,
            })
        );
    }
}
