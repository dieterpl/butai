//! Turning what ssh says into dialogs.
//!
//! Two directions. A *question* arrives through [`crate::askpass`] as the text
//! ssh would have printed at a terminal; [`ask_dialog`] reads it and builds the
//! window that asks it. A *failure* arrives as the stderr of an ssh that
//! exited; [`failure_dialog`] builds the window that explains it.
//!
//! Both work from ssh's own wording, which is stable enough across OpenSSH
//! releases to read and not stable enough to depend on: every parser here
//! falls back to showing the raw text rather than refusing, so a release that
//! rephrases a message degrades to a plainer dialog and not a lost one.

use crate::askpass::AskKind;
use crate::chrome::dialog::{Action, Button, DialogOverlay, Fact, Input, Purpose, Tone};
use crate::chrome::Role;
use crate::ssh::SshFailure;

/// The answer ssh gets for each outcome of a yes/no question.
pub fn confirm_answer(accepted: bool) -> String {
    if accepted { "yes" } else { "no" }.to_string()
}

/// What a trust question was asking, read out of ssh's text.
#[derive(Debug, PartialEq, Eq)]
struct HostKey {
    /// `macmini (100.113.243.56)`
    host: String,
    /// `ED25519`
    key_type: String,
    /// `SHA256:…`
    fingerprint: String,
    /// Anything else ssh said between the fingerprint and the question: the
    /// other names the key is known by, a DNS mismatch.
    extra: Vec<String>,
}

fn parse_host_key(prompt: &str) -> Option<HostKey> {
    if !prompt.contains("Are you sure you want to continue connecting") {
        return None;
    }
    let mut host = None;
    let mut key = None;
    let mut extra = Vec::new();
    for line in prompt.lines().map(str::trim).filter(|l| !l.is_empty()) {
        if line.starts_with("Are you sure you want to continue connecting") {
            continue;
        }
        if let Some(rest) = line.strip_prefix("The authenticity of host '") {
            // The closing quote is followed by "can't be established", whose own
            // apostrophe is why this is not a search from the right.
            host =
                rest.split_once("' ").or_else(|| rest.split_once('\'')).map(|(h, _)| h.to_string());
        } else if let Some((ty, fp)) = line.split_once(" key fingerprint is ") {
            let fp = fp.split_whitespace().next().unwrap_or_default().trim_end_matches('.');
            key = Some((ty.trim().to_string(), fp.to_string()));
        } else {
            extra.push(line.to_string());
        }
    }
    let (key_type, fingerprint) = key?;
    Some(HostKey { host: host.unwrap_or_default(), key_type, fingerprint, extra })
}

/// The window for one question ssh put to the user.
///
/// `retry` is true when ssh has asked this same thing before in this
/// connection, which for a secret means the last answer was refused — ssh says
/// so on stderr, which the helper never sees, so the repeat is the evidence.
pub fn ask_dialog(id: u64, prompt: &str, kind: AskKind, retry: bool) -> DialogOverlay {
    // OpenSSH before 8.5 does not set `SSH_ASKPASS_PROMPT`, so a yes/no arrives
    // looking like a secret; its own wording is the other tell.
    if kind == AskKind::Confirm || prompt.contains("(yes/no") {
        if let Some(h) = parse_host_key(prompt) {
            return host_key_dialog(id, h);
        }
        return DialogOverlay {
            purpose: Purpose::Ask(id),
            title: "ssh asks".into(),
            tone: Tone::Warn,
            lead: vec![prompt.trim().to_string()],
            facts: Vec::new(),
            note: Vec::new(),
            input: None,
            error: None,
            buttons: vec![
                Button::new("n", "no", Some('n'), Action::Decline),
                Button::new("y", "yes", Some('y'), Action::Accept),
            ],
            sel: 0,
        };
    }
    if kind == AskKind::Notice {
        let mut d = DialogOverlay::notice("ssh says", Tone::Info, vec![prompt.trim().to_string()]);
        d.purpose = Purpose::Ask(id);
        return d;
    }

    let lower = prompt.to_lowercase();
    let title = if lower.contains("passphrase") {
        "SSH passphrase"
    } else if lower.contains("password") {
        "SSH password"
    } else {
        "SSH authentication"
    };
    DialogOverlay {
        purpose: Purpose::Ask(id),
        title: title.into(),
        tone: Tone::Info,
        // `user@host's password: ` reads better without the trailing colon, now
        // that the input is on a row of its own.
        lead: vec![prompt.trim().trim_end_matches(':').trim_end().to_string()],
        facts: Vec::new(),
        note: Vec::new(),
        input: Some(Input::new("", true)),
        error: retry.then(|| "Permission denied, please try again.".to_string()),
        buttons: vec![
            Button::new("esc", "cancel", None, Action::Cancel),
            Button::new("enter", "ok", None, Action::Accept),
        ],
        sel: 1,
    }
}

fn host_key_dialog(id: u64, h: HostKey) -> DialogOverlay {
    let (label, value) = match h.fingerprint.strip_prefix("SHA256:") {
        Some(rest) => ("sha256", rest.to_string()),
        None => ("fingerprint", h.fingerprint.clone()),
    };
    let mut lead = vec![format!("The authenticity of {} can't be established.", h.host)];
    lead.extend(h.extra);
    let key_file = format!("ssh_host_{}_key.pub", h.key_type.to_lowercase());
    DialogOverlay {
        purpose: Purpose::Ask(id),
        title: "Unknown host".into(),
        tone: Tone::Warn,
        lead,
        facts: vec![
            Fact { label: "type".into(), value: h.key_type, role: Role::Ink },
            Fact { label: label.into(), value, role: Role::Ok },
        ],
        note: vec![
            format!("Compare it on the machine first: ssh-keygen -lf /etc/ssh/{key_file}"),
            "Trusting saves the key to ~/.ssh/known_hosts, so butai and plain ssh both accept it from then on.".into(),
        ],
        input: None,
        error: None,
        // Opens on cancel: whatever keystroke was in flight when this appeared
        // must not be the one that trusts a host.
        buttons: vec![
            Button::new("esc", "cancel", Some('n'), Action::Decline),
            Button::new("y", "trust & connect", Some('y'), Action::Accept),
        ],
        sel: 0,
    }
}

// ---- failures ----------------------------------------------------------------

/// What an ssh that exited told us, as far as a dialog needs it.
#[derive(Debug, PartialEq, Eq)]
pub enum Failure {
    /// The key the host offered is not the one saved.
    HostKeyChanged {
        host: String,
        /// SHA256 of the key it offered.
        offered: String,
        /// `known_hosts` file and line of the entry that no longer matches.
        file: String,
        line: String,
    },
    /// The user said no to a new host key, or there was no way to ask.
    HostKeyRefused,
    PermissionDenied,
    Other(String),
}

/// One line for a footer: the first line of stderr that is not a banner.
pub fn summary(stderr: &str) -> String {
    // The banner is a screenful whose first plain line is a taunt, and the footer
    // is one line.
    if stderr.contains("REMOTE HOST IDENTIFICATION HAS CHANGED") {
        return "the host key has changed".to_string();
    }
    stderr
        .lines()
        .map(str::trim)
        .find(|l| {
            !l.is_empty() && !l.starts_with('@') && !l.starts_with("Warning: Permanently added")
        })
        .unwrap_or("no output")
        .to_string()
}

pub fn parse_failure(stderr: &str) -> Failure {
    if stderr.contains("REMOTE HOST IDENTIFICATION HAS CHANGED") {
        let lines: Vec<&str> = stderr.lines().map(str::trim).collect();
        let mut offered = String::new();
        for (i, l) in lines.iter().enumerate() {
            if l.contains("sent by the remote host is") {
                let tail = l.rsplit_once(" is").map(|(_, t)| t.trim()).unwrap_or_default();
                let fp = if tail.is_empty() {
                    lines.get(i + 1).copied().unwrap_or_default()
                } else {
                    tail
                };
                offered = fp.trim_end_matches('.').to_string();
            }
        }
        let (file, line) = lines
            .iter()
            .find_map(|l| l.strip_prefix("Offending "))
            .and_then(|l| l.split_once(" key in "))
            .and_then(|(_, at)| at.rsplit_once(':'))
            .map(|(f, n)| (f.to_string(), n.to_string()))
            .unwrap_or_default();
        // ssh names the entry to remove: "ssh-keygen -f "FILE" -R "HOST"". That is
        // the exact spelling the file holds it under — `[host]:port` for a
        // non-default port — so it beats reconstructing one from the prose.
        let removal = lines.iter().find_map(|l| {
            let rest = l.strip_prefix("ssh-keygen -f \"")?;
            let (file, rest) = rest.split_once("\" -R \"")?;
            Some((file.to_string(), rest.trim_end_matches('"').to_string()))
        });
        // "Host key for X has changed" or, since OpenSSH 8.x, "ED25519 host key
        // for X has changed".
        let named = lines.iter().find_map(|l| {
            let (_, rest) = l.split_once("ey for ")?;
            rest.split_once(" has changed").map(|(h, _)| h.to_string())
        });
        let host = removal.as_ref().map(|r| r.1.clone()).or(named).unwrap_or_default();
        let file = removal.map(|r| r.0).unwrap_or(file);
        return Failure::HostKeyChanged { host, offered, file, line };
    }
    if stderr.contains("Host key verification failed") {
        return Failure::HostKeyRefused;
    }
    if stderr.contains("Permission denied") {
        return Failure::PermissionDenied;
    }
    Failure::Other(summary(stderr))
}

/// The window that explains a dial that did not arrive.
///
/// `label` is what the machine is called in the tab bar and `target` is what
/// ssh was given; they differ when a `[[remote]]` block renamed it.
pub fn failure_dialog(label: &str, target: &str, err: &anyhow::Error) -> DialogOverlay {
    let (stderr, fallback) = match err.downcast_ref::<SshFailure>() {
        Some(f) => (f.stderr.as_str(), f.summary.clone()),
        None => ("", format!("{err:#}")),
    };
    let lead = |text: String| vec![text];
    match if stderr.is_empty() { Failure::Other(fallback) } else { parse_failure(stderr) } {
        Failure::HostKeyChanged { host, offered, file, line } => {
            let at = if file.is_empty() { "known_hosts".to_string() } else { format!("{file}:{line}") };
            let name = if host.is_empty() { target.to_string() } else { host.clone() };
            let mut facts = Vec::new();
            if !offered.is_empty() {
                facts.push(Fact { label: "offered".into(), value: offered, role: Role::Attention });
            }
            facts.push(Fact { label: "saved in".into(), value: at, role: Role::Faint });
            let mut buttons = vec![Button::new("esc", "close", None, Action::Cancel)];
            // Only offered when there is a precise entry to remove: deleting
            // "whatever matched" from a file we could not read is guessing.
            let removable = !host.is_empty() && !file.is_empty();
            if removable {
                buttons.push(Button::new(
                    "R",
                    "replace saved key",
                    Some('R'),
                    Action::Other("replace"),
                ));
            }
            DialogOverlay {
                purpose: if removable {
                    Purpose::HostKeyChanged { target: target.to_string(), host, file }
                } else {
                    Purpose::Notice
                },
                title: "Host key changed".into(),
                tone: Tone::Danger,
                lead: lead(format!(
                    "The key {name} offered does not match the one saved for it. \
                     The machine may have been reinstalled or re-registered, \
                     or someone may be intercepting the connection."
                )),
                facts,
                note: vec![
                    "Butai will not connect until the saved entry is removed. Replace it only if you expected this change."
                        .into(),
                ],
                input: None,
                error: None,
                buttons,
                sel: 0,
            }
        }
        Failure::HostKeyRefused => DialogOverlay::notice(
            "Host key not trusted",
            Tone::Danger,
            lead(format!("{label} was not connected: its host key was not accepted.")),
        ),
        Failure::PermissionDenied => DialogOverlay::notice(
            format!("Cannot sign in to {label}"),
            Tone::Danger,
            vec![
                "Permission denied. ssh tried your agent, your keys in ~/.ssh and, if the machine allows it, a password.".into(),
                "Check the user name, or add the key with ssh-copy-id.".into(),
            ],
        ),
        Failure::Other(text) => DialogOverlay::notice(
            format!("Cannot reach {label}"),
            Tone::Danger,
            lead(text),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What ssh puts to askpass for the host in the bug report, verbatim.
    const UNKNOWN: &str =
        "The authenticity of host '100.113.243.56 (100.113.243.56)' can't be established.\n\
ED25519 key fingerprint is SHA256:hnZRMnSrfeIwqOHlgb106J8zCBjGGDvZgQhqkuMz6NI.\n\
This key is not known by any other names.\n\
Are you sure you want to continue connecting (yes/no/[fingerprint])? ";

    const CHANGED: &str = "@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@\n\
@    WARNING: REMOTE HOST IDENTIFICATION HAS CHANGED!     @\n\
@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@\n\
IT IS POSSIBLE THAT SOMEONE IS DOING SOMETHING NASTY!\n\
Someone could be eavesdropping on you right now (man-in-the-middle attack)!\n\
It is also possible that a host key has just been changed.\n\
The fingerprint for the ED25519 key sent by the remote host is\n\
SHA256:hnZRMnSrfeIwqOHlgb106J8zCBjGGDvZgQhqkuMz6NI.\n\
Please contact your system administrator.\n\
Add correct host key in /home/paul/.ssh/known_hosts to get rid of this message.\n\
Offending ED25519 key in /home/paul/.ssh/known_hosts:12\n\
Host key for 100.113.243.56 has changed and you have requested strict checking.\n\
Host key verification failed.\n";

    /// Verbatim from OpenSSH 8.2 against a host whose key had changed.
    const CHANGED_82: &str = "@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@\n\
@    WARNING: REMOTE HOST IDENTIFICATION HAS CHANGED!     @\n\
@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@\n\
IT IS POSSIBLE THAT SOMEONE IS DOING SOMETHING NASTY!\n\
Someone could be eavesdropping on you right now (man-in-the-middle attack)!\n\
It is also possible that a host key has just been changed.\n\
The fingerprint for the ED25519 key sent by the remote host is\n\
SHA256:Q34mI32jvHccnYc6Ah0gMIg9VxW9K9P1HCRvvaAGoNY.\n\
Please contact your system administrator.\n\
Add correct host key in /home/paul/.ssh/kno to get rid of this message.\n\
Offending ED25519 key in /home/paul/.ssh/known_hosts:2\n\
  remove with:\n\
  ssh-keygen -f \"/home/paul/.ssh/known_hosts\" -R \"[127.0.0.1]:2299\"\n\
ED25519 host key for [127.0.0.1]:2299 has changed and you have requested strict checking.\n\
Host key verification failed.\n";

    #[test]
    fn a_changed_key_names_the_exact_entry_to_remove() {
        assert_eq!(
            parse_failure(CHANGED_82),
            Failure::HostKeyChanged {
                host: "[127.0.0.1]:2299".into(),
                offered: "SHA256:Q34mI32jvHccnYc6Ah0gMIg9VxW9K9P1HCRvvaAGoNY".into(),
                file: "/home/paul/.ssh/known_hosts".into(),
                line: "2".into(),
            }
        );
    }

    #[test]
    fn reads_the_unknown_host_question() {
        let h = parse_host_key(UNKNOWN).unwrap();
        assert_eq!(h.host, "100.113.243.56 (100.113.243.56)");
        assert_eq!(h.key_type, "ED25519");
        assert_eq!(h.fingerprint, "SHA256:hnZRMnSrfeIwqOHlgb106J8zCBjGGDvZgQhqkuMz6NI");
        assert_eq!(h.extra, vec!["This key is not known by any other names."]);
    }

    #[test]
    fn a_trust_question_becomes_a_warning_that_opens_on_cancel() {
        let d = ask_dialog(7, UNKNOWN, AskKind::Confirm, false);
        assert_eq!(d.purpose, Purpose::Ask(7));
        assert_eq!(d.tone, Tone::Warn);
        assert_eq!(d.sel, 0);
        assert_eq!(d.buttons[d.sel].action, Action::Decline);
        assert!(d.facts.iter().any(|f| f.value == "hnZRMnSrfeIwqOHlgb106J8zCBjGGDvZgQhqkuMz6NI"));
        assert!(d.note[0].contains("ssh_host_ed25519_key.pub"));
    }

    #[test]
    fn an_old_ssh_that_sends_no_prompt_kind_is_still_asked_as_a_yes_no() {
        let d = ask_dialog(4, UNKNOWN, AskKind::Secret, false);
        assert!(d.input.is_none(), "a host key must never be a text field");
        assert_eq!(d.title, "Unknown host");
    }

    #[test]
    fn a_confirm_that_is_not_a_host_key_is_still_a_yes_no() {
        let d = ask_dialog(1, "Add key to the agent?", AskKind::Confirm, false);
        assert_eq!(d.lead, vec!["Add key to the agent?"]);
        assert_eq!(d.buttons.len(), 2);
        assert!(d.input.is_none());
    }

    #[test]
    fn a_passphrase_is_a_masked_input_and_a_repeat_says_why() {
        let p = "Enter passphrase for key '/home/paul/.ssh/id_ed25519': ";
        let first = ask_dialog(2, p, AskKind::Secret, false);
        assert_eq!(first.title, "SSH passphrase");
        assert!(first.input.as_ref().unwrap().secret);
        assert_eq!(first.lead, vec!["Enter passphrase for key '/home/paul/.ssh/id_ed25519'"]);
        assert!(first.error.is_none());
        assert_eq!(first.buttons[first.sel].action, Action::Accept);

        let again = ask_dialog(3, p, AskKind::Secret, true);
        assert_eq!(again.error.as_deref(), Some("Permission denied, please try again."));
    }

    #[test]
    fn a_changed_key_is_read_out_of_ssh_stderr() {
        let f = parse_failure(CHANGED);
        assert_eq!(
            f,
            Failure::HostKeyChanged {
                host: "100.113.243.56".into(),
                offered: "SHA256:hnZRMnSrfeIwqOHlgb106J8zCBjGGDvZgQhqkuMz6NI".into(),
                file: "/home/paul/.ssh/known_hosts".into(),
                line: "12".into(),
            }
        );
    }

    #[test]
    fn the_footer_line_skips_the_banner() {
        assert_eq!(summary(CHANGED), "the host key has changed");
        assert_eq!(
            summary("@@@\nPermission denied (publickey).\n"),
            "Permission denied (publickey)."
        );
        assert_eq!(summary(""), "no output");
    }

    #[test]
    fn a_changed_key_dialog_offers_to_replace_it_but_never_by_default() {
        let err = anyhow::Error::new(SshFailure { summary: "x".into(), stderr: CHANGED.into() });
        let d = failure_dialog("macmini", "macmini", &err);
        assert_eq!(d.tone, Tone::Danger);
        assert_eq!(d.buttons[d.sel].action, Action::Cancel);
        assert!(d.buttons.iter().any(|b| b.action == Action::Other("replace")));
        assert_eq!(
            d.purpose,
            Purpose::HostKeyChanged {
                target: "macmini".into(),
                host: "100.113.243.56".into(),
                file: "/home/paul/.ssh/known_hosts".into(),
            }
        );
    }

    #[test]
    fn an_unrecognised_failure_still_gets_a_window() {
        let err = anyhow::anyhow!("no answer within 20s");
        let d = failure_dialog("gpu-box", "gpu-box", &err);
        assert_eq!(d.title, "Cannot reach gpu-box");
        assert_eq!(d.lead, vec!["no answer within 20s"]);
    }
}
