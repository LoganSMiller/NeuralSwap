use std::fmt;

/// Every failure that can reach a user carries a stable machine code.
///
/// These strings are the contract shared with the frontend and with the
/// behavioural vectors under `spec/` - the messages are diagnostic detail and
/// may change freely, but a code may not.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Code {
    UnsafePath,
    ReservedName,
    SymlinkRefused,
    OutsideRoot,
    StateCorrupt,
    StateUnwritable,
    StateVersionAhead,
    JobBusy,
    JobCancelled,
    ZipInvalid,
    ZipUnsupported,
    ZipTooLarge,
    ZipEntryUnsafe,
    ZipChecksum,
    PeUnreadable,
    HardwareUnsupported,
    BadRequest,
    // Fetching a component. Separated from the install codes because none of
    // these has touched a game folder: a download that fails leaves nothing
    // behind, and the UI should offer "try again" rather than explain a state.
    /// Could not reach the server at all - DNS, connection, TLS, timeout.
    NetworkFailed,
    /// The server answered, and the answer was refused: an error status, a
    /// redirect away from HTTPS, a body larger than the cap, a truncated
    /// stream.
    DownloadRejected,
    /// The component is not something that can be fetched - it is bundled,
    /// already on the machine, or only published somewhere with no URL.
    SourceNotFetchable,
    // Install-time failures. These are the codes that can reach a user while
    // something is being written into a game folder, so each one has to say
    // enough for the UI to explain what state the folder is in.
    PackageInvalid,
    JournalCorrupt,
    TargetLocked,
    TargetProtected,
    InsufficientSpace,
    VerifyFailed,
    PlanStale,
}

impl Code {
    /// Every code, so the set can be compared against the TypeScript union
    /// rather than trusted to stay in step.
    ///
    /// It did not stay in step: `hardwareUnsupported` existed here for some
    /// time with no counterpart in `ErrorCode`, which meant the UI could
    /// receive a code it had no translation for and show a user the raw
    /// string. `spec/errors.json` now pins the two together.
    pub const ALL: [Code; 27] = [
        Code::UnsafePath,
        Code::ReservedName,
        Code::SymlinkRefused,
        Code::OutsideRoot,
        Code::StateCorrupt,
        Code::StateUnwritable,
        Code::StateVersionAhead,
        Code::JobBusy,
        Code::JobCancelled,
        Code::ZipInvalid,
        Code::ZipUnsupported,
        Code::ZipTooLarge,
        Code::ZipEntryUnsafe,
        Code::ZipChecksum,
        Code::PeUnreadable,
        Code::HardwareUnsupported,
        Code::BadRequest,
        Code::NetworkFailed,
        Code::DownloadRejected,
        Code::SourceNotFetchable,
        Code::PackageInvalid,
        Code::JournalCorrupt,
        Code::TargetLocked,
        Code::TargetProtected,
        Code::InsufficientSpace,
        Code::VerifyFailed,
        Code::PlanStale,
    ];

    /// The wire form. Must match the TypeScript `ErrorCode` union exactly.
    pub const fn as_str(self) -> &'static str {
        match self {
            Code::UnsafePath => "unsafePath",
            Code::ReservedName => "reservedName",
            Code::SymlinkRefused => "symlinkRefused",
            Code::OutsideRoot => "outsideRoot",
            Code::StateCorrupt => "stateCorrupt",
            Code::StateUnwritable => "stateUnwritable",
            Code::StateVersionAhead => "stateVersionAhead",
            Code::JobBusy => "jobBusy",
            Code::JobCancelled => "jobCancelled",
            Code::ZipInvalid => "zipInvalid",
            Code::ZipUnsupported => "zipUnsupported",
            Code::ZipTooLarge => "zipTooLarge",
            Code::ZipEntryUnsafe => "zipEntryUnsafe",
            Code::ZipChecksum => "zipChecksum",
            Code::PeUnreadable => "peUnreadable",
            Code::HardwareUnsupported => "hardwareUnsupported",
            Code::BadRequest => "badRequest",
            Code::NetworkFailed => "networkFailed",
            Code::DownloadRejected => "downloadRejected",
            Code::SourceNotFetchable => "sourceNotFetchable",
            Code::PackageInvalid => "packageInvalid",
            Code::JournalCorrupt => "journalCorrupt",
            Code::TargetLocked => "targetLocked",
            Code::TargetProtected => "targetProtected",
            Code::InsufficientSpace => "insufficientSpace",
            Code::VerifyFailed => "verifyFailed",
            Code::PlanStale => "planStale",
        }
    }
}

impl fmt::Display for Code {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug)]
pub struct Error {
    pub code: Code,
    pub detail: String,
}

impl Error {
    pub fn new(code: Code, detail: impl Into<String>) -> Self {
        Self {
            code,
            detail: detail.into(),
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.detail.is_empty() {
            f.write_str(self.code.as_str())
        } else {
            write!(f, "{}: {}", self.code.as_str(), self.detail)
        }
    }
}

impl std::error::Error for Error {}

pub type Result<T> = std::result::Result<T, Error>;

/// Shorthand for the common `return Err(...)` at a guard.
pub fn fail<T>(code: Code, detail: impl Into<String>) -> Result<T> {
    Err(Error::new(code, detail))
}

/// Serialises as just the code plus a message, so the frontend receives the
/// same `{ code, message }` shape the Electron build sent over IPC.
impl serde::Serialize for Error {
    fn serialize<S: serde::Serializer>(
        &self,
        serializer: S,
    ) -> std::result::Result<S::Ok, S::Error> {
        use serde::ser::SerializeStruct;
        let mut row = serializer.serialize_struct("Error", 2)?;
        row.serialize_field("code", self.code.as_str())?;
        row.serialize_field("message", &self.detail)?;
        row.end()
    }
}
