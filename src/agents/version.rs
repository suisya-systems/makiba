//! The Claude Code versions makiba supports. A separate constant from the base's: Claude Code is
//! not behind the seam and updates itself. The range runs from the lowest to the highest version
//! a recorded run of the probes covers (`claude --version` column, docs/design/probes.md,
//! Results), and no wider. See docs/upgrading-claude-code.md.

use std::fmt;

/// The supported range, as written in messages.
pub const SUPPORTED: &str = ">=2.1.326,<=2.1.326";
const LOWEST: (u64, u64, u64) = (2, 1, 326);
const HIGHEST: (u64, u64, u64) = (2, 1, 326);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentVersionError {
    /// The text could not be read as a version.
    Unparsable {
        text: String,
    },
    OutOfRange {
        found: String,
    },
}

impl fmt::Display for AgentVersionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unparsable { text } => write!(
                f,
                "cannot read a Claude Code version from {text:?}; makiba supports {SUPPORTED}"
            ),
            Self::OutOfRange { found } => {
                write!(
                    f,
                    "Claude Code version {found:?} is not supported; makiba supports {SUPPORTED}"
                )
            }
        }
    }
}

impl std::error::Error for AgentVersionError {}

/// Check the text `claude --version` prints, such as `2.1.326 (Claude Code)`: its first word is the version.
pub fn check(text: &str) -> Result<(), AgentVersionError> {
    let word = text.split_whitespace().next().unwrap_or("");
    let nums: Vec<u64> = word.split('.').map_while(|p| p.parse().ok()).collect();
    let [a, b, c] = nums[..] else {
        return Err(AgentVersionError::Unparsable {
            text: text.trim().to_string(),
        });
    };
    if word.split('.').count() != 3 {
        return Err(AgentVersionError::Unparsable {
            text: text.trim().to_string(),
        });
    }
    if (LOWEST..=HIGHEST).contains(&(a, b, c)) {
        Ok(())
    } else {
        Err(AgentVersionError::OutOfRange {
            found: text.trim().to_string(),
        })
    }
}
