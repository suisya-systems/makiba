//! Claude Code. Only its startup-dialog policy list exists so far (spec section 7, Issue M3-5);
//! the `Agent` for the `claude` program is M4.

use super::DialogPolicy;

/// The policy list of spec section 7, as P10 recorded the dialogs on Claude Code 2.1.326 and the
/// statuses shown with them on 2.1.327 (its second run). Changed only by a reviewed commit.
pub const DIALOGS: [DialogPolicy; 2] = [
    DialogPolicy {
        name: "folder-trust",
        options: &["No, exit", "Yes, I trust this folder"],
        choose: "Yes, I trust this folder",
        status: Some("trust this folder"),
    },
    DialogPolicy {
        name: "dev-channel",
        options: &["I am using this for local development", "Exit"],
        choose: "I am using this for local development",
        status: None,
    },
];
