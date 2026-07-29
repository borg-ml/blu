use crate::{ByteSpan, SemanticProfile};
use core::fmt;

const MAX_DIAGNOSTIC_CODE_BYTES: usize = 64;

#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct DiagnosticCode(String);

impl DiagnosticCode {
    pub fn new(value: impl Into<String>) -> Result<Self, DiagnosticCodeError> {
        let value = value.into();
        if value.is_empty() {
            return Err(DiagnosticCodeError::Empty);
        }
        if value.len() > MAX_DIAGNOSTIC_CODE_BYTES {
            return Err(DiagnosticCodeError::TooLong {
                actual: value.len(),
                limit: MAX_DIAGNOSTIC_CODE_BYTES,
            });
        }
        if !value.as_bytes()[0].is_ascii_uppercase() {
            return Err(DiagnosticCodeError::InvalidStart {
                byte: value.as_bytes()[0],
            });
        }
        if let Some((index, byte)) = value.bytes().enumerate().find(|(_, byte)| {
            !(byte.is_ascii_uppercase() || byte.is_ascii_digit() || *byte == b'-')
        }) {
            return Err(DiagnosticCodeError::InvalidByte { index, byte });
        }
        Ok(Self(value))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for DiagnosticCode {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DiagnosticCodeError {
    Empty,
    TooLong { actual: usize, limit: usize },
    InvalidStart { byte: u8 },
    InvalidByte { index: usize, byte: u8 },
}

impl fmt::Display for DiagnosticCodeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => formatter.write_str("diagnostic code is empty"),
            Self::TooLong { actual, limit } => write!(
                formatter,
                "diagnostic code contains {actual} bytes, exceeding limit {limit}"
            ),
            Self::InvalidStart { byte } => write!(
                formatter,
                "diagnostic code must start with an uppercase ASCII letter, found byte {byte}"
            ),
            Self::InvalidByte { index, byte } => write!(
                formatter,
                "diagnostic code contains invalid byte {byte} at offset {index}"
            ),
        }
    }
}

impl std::error::Error for DiagnosticCodeError {}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
#[non_exhaustive]
pub enum Phase {
    Source,
    Lex,
    Parse,
    Resolve,
    TypeCheck,
    Lower,
    Codegen,
    Validate,
    Load,
    Runtime,
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
#[non_exhaustive]
pub enum Severity {
    Error,
    Warning,
    Note,
    Help,
}

#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct Label {
    span: ByteSpan,
    message: String,
}

impl Label {
    #[must_use]
    pub fn new(span: ByteSpan, message: impl Into<String>) -> Self {
        Self {
            span,
            message: message.into(),
        }
    }

    #[must_use]
    pub const fn span(&self) -> ByteSpan {
        self.span
    }

    #[must_use]
    pub fn message(&self) -> &str {
        &self.message
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Diagnostic {
    code: DiagnosticCode,
    phase: Phase,
    profile: SemanticProfile,
    severity: Severity,
    primary: Label,
    secondary: Vec<Label>,
    expected: Vec<String>,
    found: Option<Vec<u8>>,
    notes: Vec<String>,
    help: Vec<String>,
}

impl Diagnostic {
    #[must_use]
    pub fn new(
        code: DiagnosticCode,
        phase: Phase,
        profile: SemanticProfile,
        severity: Severity,
        primary: Label,
    ) -> Self {
        Self {
            code,
            phase,
            profile,
            severity,
            primary,
            secondary: Vec::new(),
            expected: Vec::new(),
            found: None,
            notes: Vec::new(),
            help: Vec::new(),
        }
    }

    #[must_use]
    pub fn with_secondary(mut self, label: Label) -> Self {
        insert_sorted_unique(&mut self.secondary, label);
        self
    }

    #[must_use]
    pub fn with_expected(mut self, expected: impl Into<String>) -> Self {
        insert_sorted_unique(&mut self.expected, expected.into());
        self
    }

    #[must_use]
    pub fn with_found(mut self, found: impl Into<Vec<u8>>) -> Self {
        self.found = Some(found.into());
        self
    }

    #[must_use]
    pub fn with_note(mut self, note: impl Into<String>) -> Self {
        insert_sorted_unique(&mut self.notes, note.into());
        self
    }

    #[must_use]
    pub fn with_help(mut self, help: impl Into<String>) -> Self {
        insert_sorted_unique(&mut self.help, help.into());
        self
    }

    #[must_use]
    pub fn code(&self) -> &DiagnosticCode {
        &self.code
    }

    #[must_use]
    pub const fn phase(&self) -> Phase {
        self.phase
    }

    #[must_use]
    pub const fn profile(&self) -> SemanticProfile {
        self.profile
    }

    #[must_use]
    pub const fn severity(&self) -> Severity {
        self.severity
    }

    #[must_use]
    pub const fn primary(&self) -> &Label {
        &self.primary
    }

    #[must_use]
    pub fn secondary(&self) -> &[Label] {
        &self.secondary
    }

    #[must_use]
    pub fn expected(&self) -> &[String] {
        &self.expected
    }

    #[must_use]
    pub fn found(&self) -> Option<&[u8]> {
        self.found.as_deref()
    }

    #[must_use]
    pub fn notes(&self) -> &[String] {
        &self.notes
    }

    #[must_use]
    pub fn help(&self) -> &[String] {
        &self.help
    }
}

fn insert_sorted_unique<T: Ord>(values: &mut Vec<T>, value: T) {
    match values.binary_search(&value) {
        Ok(_) => {}
        Err(index) => values.insert(index, value),
    }
}
