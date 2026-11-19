//! The base: the only code that talks to herdr-core (spec section 6). This holds the `Base` trait
//! (spec section 6.1), its types, the error type and the version check. `stub` is the in-process
//! stand-in the tests use; the real implementation is `herdr.rs`.

use std::collections::BTreeMap;
use std::fmt;
use std::path::{Path, PathBuf};
use std::time::Duration;

mod herdr;
pub mod stub;
pub mod uses;
pub mod version;

/// The real base, reached at `socket`; every call has `timeout` as its read and write timeout.
pub fn connect(socket: &Path, timeout: Duration) -> Box<dyn Base> {
    Box::new(herdr::Herdr::new(socket.to_path_buf(), timeout))
}

/// As `connect`, acting for the organization `org`: `launch` tags every terminal with it and
/// `list` returns only the terminals that carry it.
pub fn connect_as(socket: &Path, timeout: Duration, org: &str) -> Box<dyn Base> {
    Box::new(herdr::Herdr::new(socket.to_path_buf(), timeout).for_organization(org))
}

/// As `connect_as`, for the real base of `session`.
pub fn connect_session_as(session: &str, timeout: Duration, org: &str) -> Box<dyn Base> {
    let runtime = std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from);
    connect_as(
        &herdr::socket_path(runtime.as_deref(), session),
        timeout,
        org,
    )
}

/// What a role must not reach on the real base (spec section 6.2): its commands, its socket folder
/// and its folders. The real base's `Base::worker_denies` returns the same; the hook
/// `makiba hook deny-base` and the role settings are checked against it.
pub fn worker_denies() -> BaseDenies {
    let runtime = std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from);
    herdr::denies(runtime.as_deref())
}

/// The real base of `session`, at `$XDG_RUNTIME_DIR/herdr-core/<session>.sock`.
pub fn connect_session(session: &str, timeout: Duration) -> Box<dyn Base> {
    let runtime = std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from);
    connect(&herdr::socket_path(runtime.as_deref(), session), timeout)
}

/// The base's own account of an error (spec section 6.1).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Detail {
    pub kind: String,
    pub message: String,
    pub hint: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BaseError {
    NotFound(Detail),
    Blocked(Detail),
    StatusMismatch(Detail),
    Finished(Detail),
    Unavailable(Detail),
    UnsupportedVersion { found: String, supported: String },
    Other(Detail),
}

impl fmt::Display for BaseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            BaseError::UnsupportedVersion { found, supported } => write!(
                f,
                "herdr-core version {found:?} is not supported; makiba supports {supported}"
            ),
            BaseError::NotFound(d)
            | BaseError::Blocked(d)
            | BaseError::StatusMismatch(d)
            | BaseError::Finished(d)
            | BaseError::Unavailable(d)
            | BaseError::Other(d) => write!(f, "{}: {}", d.kind, d.message),
        }
    }
}

impl std::error::Error for BaseError {}

/// What `Base::hello()` returns.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BaseInfo {
    pub name: String,
    pub version: String,
}

/// How a terminal is launched. `env` carries no key: the agent's token is in `argv` (spec section 4).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LaunchSpec {
    pub argv: Vec<String>,
    pub cwd: PathBuf,
    pub name: String,
    pub tags: BTreeMap<String, String>,
    pub env: BTreeMap<String, String>,
    pub unset: Vec<String>,
}

/// The base's own handle for a terminal.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TerminalRef(pub String);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Terminal {
    pub id: TerminalRef,
    pub name: String,
    pub tags: BTreeMap<String, String>,
    /// `None` while the program has not reported.
    pub status: Option<Status>,
    pub turn: Option<u64>,
    pub alive: bool,
    pub exit_code: Option<i32>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Status {
    pub state: State,
    pub kind: Option<BlockKind>,
    pub msg: Option<String>,
}

/// As in OSC 7501.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    Idle,
    Working,
    Blocked,
    Done,
    Error,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlockKind {
    Permission,
    Question,
    Auth,
}

/// What happened to one terminal. Each carries the terminal as it is after the change.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TerminalEvent {
    Created(Terminal),
    StatusChanged(Terminal),
    Exited(Terminal),
    Removed(TerminalRef),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BaseEvent {
    Event(TerminalEvent),
    /// A fresh `list`, yielded first and whenever the stream cannot be sure it saw everything.
    Resync(Vec<Terminal>),
}

/// Never ends while the process runs: the implementation reconnects by itself and yields a Resync.
pub type EventStream = Box<dyn Iterator<Item = BaseEvent> + Send>;

/// What a role must not reach: commands, socket paths and folders (spec section 6.2).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BaseDenies {
    /// Command names, matched as a command word or as the last component of its path.
    pub commands: Vec<String>,
    /// Resolved in this process. A static role file cannot name them; the per-task settings
    /// written at launch do (M4-2).
    pub sockets: Vec<PathBuf>,
    /// In the `~/` form Claude Code's permission rules and sandbox paths both read as the home.
    pub folders: Vec<PathBuf>,
}

/// The one interface to the base (spec section 6.1). Used from several threads, so methods take
/// `&self`. `stub::Stub` shows what each method means.
pub trait Base: Send + Sync {
    /// `"herdr-core"` or `"stub"`.
    fn name(&self) -> &'static str;
    /// The version, refused unless supported: `Unavailable`, `UnsupportedVersion`.
    fn hello(&self) -> Result<BaseInfo, BaseError>;
    fn launch(&self, spec: &LaunchSpec) -> Result<TerminalRef, BaseError>;
    /// Only terminals tagged with this organization.
    fn list(&self) -> Result<Vec<Terminal>, BaseError>;
    fn get(&self, t: &TerminalRef) -> Result<Terminal, BaseError>;
    fn read(&self, t: &TerminalRef, lines: usize) -> Result<Vec<String>, BaseError>;
    fn keys(&self, t: &TerminalRef, keys: &[&str]) -> Result<(), BaseError>;
    fn send(&self, t: &TerminalRef, text: &str, only_if_idle: bool) -> Result<(), BaseError>;
    /// Kill and remove.
    fn end(&self, t: &TerminalRef) -> Result<(), BaseError>;
    /// Its own connection (spec section 6.5).
    fn events(&self) -> Result<EventStream, BaseError>;
    /// Makiba up: start the base's own place if needed.
    fn start(&self) -> Result<(), BaseError>;
    fn worker_denies(&self) -> BaseDenies;
}
