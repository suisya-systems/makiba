//! The Claude Code versions makiba supports. A separate constant from the base's: Claude Code is
//! not behind the seam and updates itself, so there is no upper bound. Every version at or above
//! `LOWEST` is accepted except those named in `REFUSED`. `NEWEST_CHECKED` is the newest version a
//! recorded run of the probes covers (`claude --version` column, docs/design/probes.md, Results);
//! it is information only. See docs/upgrading-claude-code.md.

use std::fmt;

type Version = (u64, u64, u64);

/// The supported versions, as written in messages.
pub const SUPPORTED: &str = ">=2.1.326";
/// The lowest supported version. Public so that tests build the edges from it.
pub const LOWEST: Version = (2, 1, 326);
/// The newest version the organisation has run the probes on. Not a bound.
pub const NEWEST_CHECKED: Version = (2, 1, 328);
/// Versions refused by name, each with a one-line reason. Empty today.
pub const REFUSED: &[(Version, &str)] = &[];

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentVersionError {
    /// The text could not be read as a version.
    Unparsable { text: String },
    /// Below the lowest supported version.
    TooOld { found: String },
    /// Named on the refused list.
    Refused { found: String, reason: String },
}

impl fmt::Display for AgentVersionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unparsable { text } => write!(
                f,
                "cannot read a Claude Code version from {text:?}; makiba supports {SUPPORTED}"
            ),
            Self::TooOld { found } => write!(
                f,
                "Claude Code version {found:?} is below the lowest supported version; makiba supports {SUPPORTED}"
            ),
            Self::Refused { found, reason } => {
                write!(f, "Claude Code version {found:?} is refused: {reason}")
            }
        }
    }
}

impl std::error::Error for AgentVersionError {}

/// Check the text `claude --version` prints, such as `2.1.326 (Claude Code)`: its first word is
/// the version. `Ok(Some(line))` is an accepted version newer than `NEWEST_CHECKED`: the line to
/// print before going on.
pub fn check(text: &str) -> Result<Option<String>, AgentVersionError> {
    check_with(text, REFUSED)
}

/// `check` against a given refused list (the seam for tests; `check` passes `REFUSED`).
pub fn check_with(
    text: &str,
    refused: &[(Version, &str)],
) -> Result<Option<String>, AgentVersionError> {
    let word = text.split_whitespace().next().unwrap_or("");
    let nums: Vec<u64> = word.split('.').map_while(|p| p.parse().ok()).collect();
    let unparsable = || AgentVersionError::Unparsable {
        text: text.trim().to_string(),
    };
    let [a, b, c] = nums[..] else {
        return Err(unparsable());
    };
    if word.split('.').count() != 3 {
        return Err(unparsable());
    }
    let found = text.trim().to_string();
    let v = (a, b, c);
    if v < LOWEST {
        return Err(AgentVersionError::TooOld { found });
    }
    if let Some((_, reason)) = refused.iter().find(|(r, _)| *r == v) {
        return Err(AgentVersionError::Refused {
            found,
            reason: reason.to_string(),
        });
    }
    if v > NEWEST_CHECKED {
        let (x, y, z) = NEWEST_CHECKED;
        return Ok(Some(format!(
            "Claude Code {found:?} is newer than {x}.{y}.{z}, the newest version the probes were run on; going on"
        )));
    }
    Ok(None)
}
