//! Asking the person before a connected program's write lands (ADR-053).
//!
//! A program the person set to "ask first" has each write held: the host
//! builds the bytes, hands a [`WriteProposal`] to a [`WriteReview`], and lands
//! them only on [`ReviewAnswer::Apply`]. The proposal and the answer travel as
//! files between `jotra mcp` and the app (`jotra_storage::write_review`); this
//! module is their shape and the rule that anything but Apply writes nothing.

use std::time::Duration;

use serde::{Deserialize, Serialize};

/// How long a write waits for the person's answer before it gives up and
/// writes nothing.
pub const REVIEW_WAIT: Duration = Duration::from_secs(5 * 60);

/// How long Codex waits for one tool call before it gives up on it, unless
/// its own `tool_timeout_sec` setting says otherwise.
pub const CODEX_TOOL_TIMEOUT: Duration = Duration::from_secs(60);

/// How much sooner than a program's own limit its held write stops waiting,
/// so the program reads the answer before it gives up on the call.
pub const PROGRAM_LIMIT_MARGIN: Duration = Duration::from_secs(5);

/// The programs, by the name each sends, that give up on a tool call sooner
/// than [`REVIEW_WAIT`], with how long each waits.
pub const PROGRAM_TOOL_LIMITS: &[(&str, Duration)] = &[
    ("codex", CODEX_TOOL_TIMEOUT),
    ("codex-cli", CODEX_TOOL_TIMEOUT),
    ("codex-mcp-client", CODEX_TOOL_TIMEOUT),
];

/// How long a held write from `program` waits for the person: [`REVIEW_WAIT`],
/// or less than the program's own limit for one in [`PROGRAM_TOOL_LIMITS`], so
/// nothing lands after the program has stopped waiting for the call.
pub fn review_wait_for(program: &str) -> Duration {
    PROGRAM_TOOL_LIMITS
        .iter()
        .find(|&&(name, _)| name == program)
        .map_or(REVIEW_WAIT, |&(_, limit)| {
            limit.saturating_sub(PROGRAM_LIMIT_MARGIN).min(REVIEW_WAIT)
        })
}

/// What a held write would do to the file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ProposedChange {
    /// The file's text, before and after.
    Edit {
        /// What the file holds, as the write was built from it.
        before: String,
        /// What the write would leave there.
        after: String,
    },
    /// A file that does not exist yet.
    Create {
        /// What it would hold.
        after: String,
    },
    /// A new name for the file, in the folder it is in.
    Rename {
        /// The name it would take.
        to: String,
    },
}

/// One held write: which program, which file, and what it would change.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WriteProposal {
    /// The program, by the name it sent.
    pub program: String,
    /// The file, as the host resolved it.
    pub path: String,
    /// What the write would do.
    pub change: ProposedChange,
}

/// The answer to one held write.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReviewAnswer {
    /// The person applied it: the write goes ahead, still under the guard.
    Apply,
    /// The person discarded it: nothing is written.
    Discard,
    /// Jotra could not show it, because its window was not ready.
    NotShown,
}

/// Why a held write was not answered.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ReviewUnavailable {
    /// Jotra is not running, so there is nobody to ask.
    #[error("Jotra is not running")]
    NotRunning,
    /// Jotra could not show the change.
    #[error("Jotra could not show the change")]
    NotShown,
    /// Nobody answered within [`REVIEW_WAIT`].
    #[error("nobody answered in time")]
    Unanswered,
}

/// Who a held write is shown to.
pub trait WriteReview: Send + Sync {
    /// Shows `proposal` and waits for the answer.
    ///
    /// # Errors
    ///
    /// [`ReviewUnavailable`] when nobody could be asked or nobody answered.
    /// Every error means nothing is written.
    fn review(&self, proposal: &WriteProposal) -> Result<ReviewAnswer, ReviewUnavailable>;
}

/// What the host does with a review's outcome: `Ok` to write, the reason not
/// to otherwise.
///
/// # Errors
///
/// [`Refused::Discarded`] for a discard, [`Refused::Unavailable`] when the
/// change was not shown or not answered.
pub fn verdict(outcome: Result<ReviewAnswer, ReviewUnavailable>) -> Result<(), Refused> {
    match outcome {
        Ok(ReviewAnswer::Apply) => Ok(()),
        Ok(ReviewAnswer::Discard) => Err(Refused::Discarded),
        Ok(ReviewAnswer::NotShown) => Err(Refused::Unavailable(ReviewUnavailable::NotShown)),
        Err(reason) => Err(Refused::Unavailable(reason)),
    }
}

/// Why a held write was not written.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refused {
    /// The person discarded it.
    Discarded,
    /// Nobody could be asked, or nobody answered.
    Unavailable(ReviewUnavailable),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_apply_writes() {
        assert_eq!(verdict(Ok(ReviewAnswer::Apply)), Ok(()));
        assert_eq!(verdict(Ok(ReviewAnswer::Discard)), Err(Refused::Discarded));
        assert_eq!(
            verdict(Ok(ReviewAnswer::NotShown)),
            Err(Refused::Unavailable(ReviewUnavailable::NotShown))
        );
        for reason in [
            ReviewUnavailable::NotRunning,
            ReviewUnavailable::NotShown,
            ReviewUnavailable::Unanswered,
        ] {
            assert_eq!(verdict(Err(reason)), Err(Refused::Unavailable(reason)));
        }
    }

    #[test]
    fn a_program_that_gives_up_on_a_call_sooner_is_answered_before_it_does() {
        for codex in ["codex", "codex-cli", "codex-mcp-client"] {
            let wait = review_wait_for(codex);
            assert!(wait < CODEX_TOOL_TIMEOUT, "{codex} waits {wait:?}");
            assert!(wait >= Duration::from_secs(30), "{codex} waits {wait:?}");
        }
        assert_eq!(review_wait_for("claude-code"), REVIEW_WAIT);
        assert_eq!(review_wait_for("Codex"), REVIEW_WAIT);
    }

    #[test]
    fn a_proposal_and_an_answer_keep_their_wire_shape() {
        let proposal = WriteProposal {
            program: "claude-code".to_string(),
            path: "/notes/Plan.md".to_string(),
            change: ProposedChange::Edit {
                before: "a\n".to_string(),
                after: "b\n".to_string(),
            },
        };
        let json = serde_json::to_value(&proposal).expect("serialize");
        assert_eq!(json["change"]["kind"], "edit");
        let back: WriteProposal = serde_json::from_value(json).expect("deserialize");
        assert_eq!(back, proposal);
        assert_eq!(
            serde_json::to_string(&ReviewAnswer::NotShown).expect("serialize"),
            "\"not_shown\""
        );
        let rename = serde_json::to_value(ProposedChange::Rename {
            to: "Launch.md".to_string(),
        })
        .expect("serialize");
        assert_eq!(
            rename,
            serde_json::json!({"kind": "rename", "to": "Launch.md"})
        );
    }
}
