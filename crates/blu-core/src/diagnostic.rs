use crate::{ByteSpan, SemanticProfile};
use core::fmt;

const MAX_DIAGNOSTIC_CODE_BYTES: usize = 64;

#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct DiagnosticCode(String);

impl DiagnosticCode {
    pub fn new(value: impl AsRef<str>) -> Result<Self, DiagnosticCodeError> {
        let value = value.as_ref();
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
        let mut owned = String::new();
        owned.try_reserve_exact(value.len()).map_err(|_| {
            DiagnosticCodeError::AllocationFailed {
                requested_bytes: value.len(),
            }
        })?;
        owned.push_str(value);
        Ok(Self(owned))
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
    AllocationFailed { requested_bytes: usize },
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
            Self::AllocationFailed { requested_bytes } => write!(
                formatter,
                "failed to allocate {requested_bytes} bytes for a diagnostic code"
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

/// Per-diagnostic ownership limits. String byte limits apply to each item.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DiagnosticLimits {
    pub max_label_message_bytes: usize,
    pub max_secondary_labels: usize,
    pub max_expected_items: usize,
    pub max_expected_item_bytes: usize,
    pub max_found_bytes: usize,
    pub max_notes: usize,
    pub max_note_bytes: usize,
    pub max_help_items: usize,
    pub max_help_item_bytes: usize,
}

impl Default for DiagnosticLimits {
    fn default() -> Self {
        Self {
            max_label_message_bytes: 1_024,
            max_secondary_labels: 32,
            max_expected_items: 32,
            max_expected_item_bytes: 256,
            max_found_bytes: 32,
            max_notes: 32,
            max_note_bytes: 1_024,
            max_help_items: 32,
            max_help_item_bytes: 1_024,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DiagnosticLimit {
    LabelMessageBytes,
    SecondaryLabels,
    ExpectedItems,
    ExpectedItemBytes,
    FoundBytes,
    Notes,
    NoteBytes,
    HelpItems,
    HelpItemBytes,
}

impl fmt::Display for DiagnosticLimit {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::LabelMessageBytes => formatter.write_str("diagnostic label message bytes"),
            Self::SecondaryLabels => formatter.write_str("diagnostic secondary labels"),
            Self::ExpectedItems => formatter.write_str("diagnostic expected items"),
            Self::ExpectedItemBytes => formatter.write_str("diagnostic expected-item bytes"),
            Self::FoundBytes => formatter.write_str("diagnostic found bytes"),
            Self::Notes => formatter.write_str("diagnostic notes"),
            Self::NoteBytes => formatter.write_str("diagnostic note bytes"),
            Self::HelpItems => formatter.write_str("diagnostic help items"),
            Self::HelpItemBytes => formatter.write_str("diagnostic help-item bytes"),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DiagnosticError {
    Code(DiagnosticCodeError),
    Limit {
        kind: DiagnosticLimit,
        required: usize,
        limit: usize,
    },
    SizeOverflow {
        what: &'static str,
    },
    AllocationFailed {
        what: &'static str,
        requested: usize,
    },
}

impl fmt::Display for DiagnosticError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Code(error) => error.fmt(formatter),
            Self::Limit {
                kind,
                required,
                limit,
            } => write!(
                formatter,
                "{kind} require {required}, exceeding limit {limit}"
            ),
            Self::SizeOverflow { what } => write!(formatter, "{what} size overflowed"),
            Self::AllocationFailed { what, requested } => {
                write!(formatter, "failed to allocate {what} for {requested}")
            }
        }
    }
}

impl std::error::Error for DiagnosticError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Code(error) => Some(error),
            Self::Limit { .. } | Self::SizeOverflow { .. } | Self::AllocationFailed { .. } => None,
        }
    }
}

impl From<DiagnosticCodeError> for DiagnosticError {
    fn from(error: DiagnosticCodeError) -> Self {
        Self::Code(error)
    }
}

#[derive(Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct Label {
    span: ByteSpan,
    message: String,
}

impl Label {
    fn try_new(
        span: ByteSpan,
        message: &str,
        limits: DiagnosticLimits,
    ) -> Result<Self, DiagnosticError> {
        check_limit(
            DiagnosticLimit::LabelMessageBytes,
            message.len(),
            limits.max_label_message_bytes,
        )?;
        Ok(Self {
            span,
            message: copy_string(message, "diagnostic label message bytes")?,
        })
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

#[derive(Debug)]
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
    limits: DiagnosticLimits,
}

impl PartialEq for Diagnostic {
    fn eq(&self, other: &Self) -> bool {
        self.code == other.code
            && self.phase == other.phase
            && self.profile == other.profile
            && self.severity == other.severity
            && self.primary == other.primary
            && self.secondary == other.secondary
            && self.expected == other.expected
            && self.found == other.found
            && self.notes == other.notes
            && self.help == other.help
    }
}

impl Eq for Diagnostic {}

impl Diagnostic {
    /// Constructs a bounded diagnostic from borrowed text.
    pub fn try_new(
        code: &str,
        phase: Phase,
        profile: SemanticProfile,
        severity: Severity,
        primary_span: ByteSpan,
        primary_message: &str,
        limits: DiagnosticLimits,
    ) -> Result<Self, DiagnosticError> {
        Ok(Self {
            code: DiagnosticCode::new(code)?,
            phase,
            profile,
            severity,
            primary: Label::try_new(primary_span, primary_message, limits)?,
            secondary: Vec::new(),
            expected: Vec::new(),
            found: None,
            notes: Vec::new(),
            help: Vec::new(),
            limits,
        })
    }

    pub fn try_with_secondary(
        mut self,
        span: ByteSpan,
        message: &str,
    ) -> Result<Self, DiagnosticError> {
        let label = Label::try_new(span, message, self.limits)?;
        if let Err(index) = self.secondary.binary_search(&label) {
            check_limit(
                DiagnosticLimit::SecondaryLabels,
                self.secondary.len().saturating_add(1),
                self.limits.max_secondary_labels,
            )?;
            reserve_one(&mut self.secondary, "diagnostic secondary labels")?;
            self.secondary.insert(index, label);
        }
        Ok(self)
    }

    pub fn try_with_expected(mut self, expected: &str) -> Result<Self, DiagnosticError> {
        try_insert_string(
            &mut self.expected,
            expected,
            DiagnosticLimit::ExpectedItems,
            self.limits.max_expected_items,
            DiagnosticLimit::ExpectedItemBytes,
            self.limits.max_expected_item_bytes,
            "diagnostic expected items",
            "diagnostic expected-item bytes",
        )?;
        Ok(self)
    }

    pub fn try_with_found(mut self, found: &[u8]) -> Result<Self, DiagnosticError> {
        check_limit(
            DiagnosticLimit::FoundBytes,
            found.len(),
            self.limits.max_found_bytes,
        )?;
        let mut owned = Vec::new();
        owned
            .try_reserve_exact(found.len())
            .map_err(|_| DiagnosticError::AllocationFailed {
                what: "diagnostic found bytes",
                requested: found.len(),
            })?;
        owned.extend_from_slice(found);
        self.found = Some(owned);
        Ok(self)
    }

    pub fn try_with_note(self, note: &str) -> Result<Self, DiagnosticError> {
        self.try_with_note_parts(&[note])
    }

    pub fn try_with_note_parts(mut self, parts: &[&str]) -> Result<Self, DiagnosticError> {
        let note = copy_string_parts(
            parts,
            DiagnosticLimit::NoteBytes,
            self.limits.max_note_bytes,
            "diagnostic note bytes",
        )?;
        try_insert_owned(
            &mut self.notes,
            note,
            DiagnosticLimit::Notes,
            self.limits.max_notes,
            "diagnostic notes",
        )?;
        Ok(self)
    }

    pub fn try_with_help(mut self, help: &str) -> Result<Self, DiagnosticError> {
        try_insert_string(
            &mut self.help,
            help,
            DiagnosticLimit::HelpItems,
            self.limits.max_help_items,
            DiagnosticLimit::HelpItemBytes,
            self.limits.max_help_item_bytes,
            "diagnostic help items",
            "diagnostic help-item bytes",
        )?;
        Ok(self)
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

    #[must_use]
    pub const fn limits(&self) -> DiagnosticLimits {
        self.limits
    }
}

fn check_limit(
    kind: DiagnosticLimit,
    required: usize,
    limit: usize,
) -> Result<(), DiagnosticError> {
    if required > limit {
        Err(DiagnosticError::Limit {
            kind,
            required,
            limit,
        })
    } else {
        Ok(())
    }
}

fn copy_string(value: &str, what: &'static str) -> Result<String, DiagnosticError> {
    let mut owned = String::new();
    owned
        .try_reserve_exact(value.len())
        .map_err(|_| DiagnosticError::AllocationFailed {
            what,
            requested: value.len(),
        })?;
    owned.push_str(value);
    Ok(owned)
}

fn copy_string_parts(
    parts: &[&str],
    kind: DiagnosticLimit,
    limit: usize,
    what: &'static str,
) -> Result<String, DiagnosticError> {
    let required = parts
        .iter()
        .try_fold(0usize, |total, part| total.checked_add(part.len()));
    let required = required.ok_or(DiagnosticError::SizeOverflow { what })?;
    check_limit(kind, required, limit)?;
    let mut owned = String::new();
    owned
        .try_reserve_exact(required)
        .map_err(|_| DiagnosticError::AllocationFailed {
            what,
            requested: required,
        })?;
    for part in parts {
        owned.push_str(part);
    }
    Ok(owned)
}

#[allow(clippy::too_many_arguments)]
fn try_insert_string(
    values: &mut Vec<String>,
    value: &str,
    count_kind: DiagnosticLimit,
    count_limit: usize,
    byte_kind: DiagnosticLimit,
    byte_limit: usize,
    values_what: &'static str,
    value_what: &'static str,
) -> Result<(), DiagnosticError> {
    if let Err(index) = values.binary_search_by(|candidate| candidate.as_str().cmp(value)) {
        check_limit(byte_kind, value.len(), byte_limit)?;
        check_limit(count_kind, values.len().saturating_add(1), count_limit)?;
        reserve_one(values, values_what)?;
        let owned = copy_string(value, value_what)?;
        values.insert(index, owned);
    }
    Ok(())
}

fn try_insert_owned(
    values: &mut Vec<String>,
    value: String,
    count_kind: DiagnosticLimit,
    count_limit: usize,
    what: &'static str,
) -> Result<(), DiagnosticError> {
    if let Err(index) = values.binary_search(&value) {
        check_limit(count_kind, values.len().saturating_add(1), count_limit)?;
        reserve_one(values, what)?;
        values.insert(index, value);
    }
    Ok(())
}

fn reserve_one<T>(values: &mut Vec<T>, what: &'static str) -> Result<(), DiagnosticError> {
    if values.len() == values.capacity() {
        let requested = values.len().saturating_add(1);
        values
            .try_reserve(1)
            .map_err(|_| DiagnosticError::AllocationFailed { what, requested })?;
    }
    Ok(())
}
