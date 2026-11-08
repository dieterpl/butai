//! Built-in template prompts and roles of your own.
//!
//! The `[prompts]` picker lists the file's `[[prompts]]` blocks *and* four
//! built-ins, one per built-in role: `coordinator` (a lead that hands work to
//! workers), `worker`, `loop` and `watch`. They follow the same rule as the
//! daemon's built-in launchers and `[[agents]]`: a block with a built-in's
//! name replaces it where it stands, any other block comes after. Deleting a
//! built-in writes `hidden = true` under its name, because a built-in with no
//! block would simply come back.
//!
//! `[[roles]]` blocks name roles beyond the four the daemon knows how to
//! colour. The daemon accepts any role word (see
//! [`butai_protocol::api::is_role_word`]), so a role of your own is only a
//! name plus what to start with it: a harness, a prompt and labels.

use serde::Deserialize;

use crate::config::PromptTemplate;

/// The built-in prompts: name, the role they start an agent as, and text.
pub const BUILTIN_PROMPTS: [(&str, &str, &str); 4] = [
    ("coordinator", "lead", include_str!("roles/coordinator.md")),
    ("worker", "worker", include_str!("roles/worker.md")),
    ("loop", "loop", include_str!("roles/loop.md")),
    ("watch", "watch", include_str!("roles/watch.md")),
];

/// Whether `name` is one of [`BUILTIN_PROMPTS`].
pub fn is_builtin_prompt(name: &str) -> bool {
    BUILTIN_PROMPTS.iter().any(|(n, ..)| *n == name)
}

/// The prompts the picker lists: the built-ins, each replaced or hidden by a
/// block of the same name, then the file's other blocks in file order.
///
/// A replacing block with no `role` keeps the built-in's, so rewording the
/// coordinator does not stop it being a lead.
pub fn prompts(file: &[PromptTemplate]) -> Vec<PromptTemplate> {
    let mut out = Vec::new();
    for (name, role, text) in BUILTIN_PROMPTS {
        match file.iter().find(|p| p.name == name) {
            Some(p) if p.hidden => {}
            Some(p) => out.push(PromptTemplate {
                role: p.role.clone().or_else(|| Some(role.into())),
                ..p.clone()
            }),
            None => out.push(PromptTemplate {
                name: name.into(),
                text: text.trim_end().into(),
                role: Some(role.into()),
                hidden: false,
            }),
        }
    }
    out.extend(file.iter().filter(|p| !p.hidden && !is_builtin_prompt(&p.name)).cloned());
    out
}

/// One `[[roles]]` block: a role of your own.
///
/// ```toml
/// [[roles]]
/// name = "reviewer"          # the role tag the agent gets
/// agent = "codex"            # optional: the harness to start
/// prompt = "review this diff" # optional: a [[prompts]] name to send
/// labels = ["review"]        # optional
/// ```
#[derive(Debug, Clone, Deserialize, Default, PartialEq, Eq)]
#[serde(default)]
pub struct RoleDef {
    pub name: String,
    pub agent: Option<String>,
    pub prompt: Option<String>,
    pub labels: Vec<String>,
}

/// A row of the spawn picker that starts an agent *as* something: a harness,
/// the role it is tagged with, its labels, and the prompt pasted once it is up.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Preset {
    /// What the row is called: the prompt's name, or the `[[roles]]` name.
    pub name: String,
    pub agent: String,
    pub role: String,
    pub labels: Vec<String>,
    /// The text to paste, already looked up. `None` starts it with no prompt.
    pub prompt: Option<String>,
}

/// Marks a picker value as a preset rather than a harness name. A unit
/// separator, because nothing a person names an agent can contain one.
const SEP: char = '\u{1f}';

impl Preset {
    /// The picker value this row carries: everything spawning it needs, so the
    /// list stays a list of strings and needs no state beside it.
    pub fn value(&self) -> String {
        let f = [
            &self.name,
            &self.agent,
            &self.role,
            &self.labels.join(","),
            self.prompt.as_deref().unwrap_or(""),
        ];
        let mut v = String::new();
        for x in f {
            v.push(SEP);
            v.push_str(x);
        }
        v
    }

    /// The inverse of [`value`](Self::value). `None` for a plain harness name.
    pub fn parse(v: &str) -> Option<Preset> {
        let mut f = v.strip_prefix(SEP)?.splitn(5, SEP);
        let (name, agent, role) = (f.next()?, f.next()?, f.next()?);
        let labels = f.next()?.split(',').filter(|l| !l.is_empty()).map(str::to_string).collect();
        let prompt = f.next().filter(|p| !p.is_empty()).map(str::to_string);
        Some(Preset { name: name.into(), agent: agent.into(), role: role.into(), labels, prompt })
    }

    /// The row as drawn: `coordinator  claude → lead #review`, plus `✎` when a
    /// prompt goes with it.
    pub fn row(&self) -> String {
        let tags: String = self.labels.iter().map(|l| format!(" #{l}")).collect();
        let pen = if self.prompt.is_some() { "  ✎" } else { "" };
        format!("{:<12} {} → {}{tags}{pen}", self.name, self.agent, self.role)
    }
}

/// The picker's preset rows: one per prompt that carries a role, then one per
/// `[[roles]]` block.
///
/// `agents` is what the daemon can start. A preset's harness is its own (a
/// `[[roles]] agent` the daemon has), else `default` (the client's pin) if the
/// daemon has it, else `claude`, else the daemon's first. A `[[roles]] prompt`
/// is a prompt's name; one that names no prompt is taken as the text itself.
/// A role that is not a role word is skipped, since the daemon would refuse it.
pub fn presets(
    prompts: &[PromptTemplate],
    roles: &[RoleDef],
    agents: &[String],
    default: Option<&str>,
) -> Vec<Preset> {
    let has = |a: &str| agents.iter().any(|n| n == a);
    let fallback = default
        .filter(|d| has(d))
        .or_else(|| has("claude").then_some("claude"))
        .or(agents.first().map(String::as_str));
    let Some(fallback) = fallback else { return Vec::new() };
    let ok = |r: &str| butai_protocol::api::is_role_word(r);
    let mut out: Vec<Preset> = prompts
        .iter()
        .filter_map(|p| {
            let role = p.role.as_deref().filter(|r| ok(r))?;
            Some(Preset {
                name: p.name.clone(),
                agent: fallback.into(),
                role: role.into(),
                labels: Vec::new(),
                prompt: Some(p.text.clone()),
            })
        })
        .collect();
    out.extend(roles.iter().filter(|r| ok(&r.name)).map(|r| Preset {
        name: r.name.clone(),
        agent: r.agent.clone().filter(|a| has(a)).unwrap_or_else(|| fallback.into()),
        role: r.name.clone(),
        labels: r.labels.clone(),
        prompt: r.prompt.as_ref().map(|name| {
            prompts
                .iter()
                .find(|p| &p.name == name)
                .map_or_else(|| name.clone(), |p| p.text.clone())
        }),
    }));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tpl(name: &str, text: &str) -> PromptTemplate {
        PromptTemplate { name: name.into(), text: text.into(), ..Default::default() }
    }

    #[test]
    fn with_no_blocks_the_four_builtins_are_listed_with_their_roles() {
        let all = prompts(&[]);
        let names: Vec<_> = all.iter().map(|p| (p.name.as_str(), p.role.as_deref())).collect();
        assert_eq!(
            names,
            [
                ("coordinator", Some("lead")),
                ("worker", Some("worker")),
                ("loop", Some("loop")),
                ("watch", Some("watch"))
            ]
        );
        assert!(all[0].text.starts_with("You are the coordinator"), "{}", all[0].text);
        assert!(all.iter().all(|p| !p.text.is_empty() && !p.text.ends_with('\n')));
    }

    #[test]
    fn a_block_with_a_builtins_name_replaces_it_in_place_and_keeps_its_role() {
        let all = prompts(&[tpl("mine", "Mine."), tpl("coordinator", "Lead, briefly.")]);
        assert_eq!(all[0].name, "coordinator");
        assert_eq!(all[0].text, "Lead, briefly.");
        assert_eq!(all[0].role.as_deref(), Some("lead"));
        assert_eq!(all.last().unwrap().name, "mine", "the file's own come after");
        assert_eq!(all.len(), 5);
        let own_role = PromptTemplate { role: Some("reviewer".into()), ..tpl("worker", "W.") };
        assert_eq!(prompts(&[own_role])[1].role.as_deref(), Some("reviewer"));
    }

    #[test]
    fn a_hidden_block_hides_the_builtin_and_nothing_else() {
        let hide = PromptTemplate { hidden: true, ..tpl("loop", "") };
        let all = prompts(&[hide, tpl("x", "X.")]);
        assert!(all.iter().all(|p| p.name != "loop"));
        assert_eq!(all.len(), 4);
    }

    fn names(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn presets_are_the_role_prompts_then_the_roles_blocks() {
        let mut file = vec![tpl("review this diff", "Review it."), tpl("plain", "No role.")];
        file.push(PromptTemplate { hidden: true, ..tpl("watch", "") });
        let reviewer = RoleDef {
            name: "reviewer".into(),
            agent: Some("codex".into()),
            prompt: Some("review this diff".into()),
            labels: vec!["review".into()],
        };
        let all = presets(&prompts(&file), &[reviewer], &names(&["codex", "claude"]), None);
        let rows: Vec<_> =
            all.iter().map(|p| (p.name.as_str(), p.agent.as_str(), p.role.as_str())).collect();
        assert_eq!(
            rows,
            [
                ("coordinator", "claude", "lead"),
                ("worker", "claude", "worker"),
                ("loop", "claude", "loop"),
                ("reviewer", "codex", "reviewer"),
            ],
            "claude is the fallback harness; plain and hidden prompts are not presets"
        );
        assert!(all[0].prompt.as_deref().unwrap().starts_with("You are the coordinator"));
        assert_eq!(all[3].prompt.as_deref(), Some("Review it."), "a prompt name is looked up");
        assert_eq!(all[3].labels, ["review"]);
    }

    #[test]
    fn a_roles_harness_the_daemon_lacks_falls_back_to_the_pin() {
        let r = RoleDef {
            name: "ops".into(),
            agent: Some("gemini".into()),
            prompt: Some("Be careful.".into()),
            ..Default::default()
        };
        let all = presets(&[], &[r], &names(&["claude", "codex"]), Some("codex"));
        assert_eq!(
            (all[0].agent.as_str(), all[0].prompt.as_deref()),
            ("codex", Some("Be careful."))
        );
        let bad = RoleDef { name: "Not A Word".into(), ..Default::default() };
        assert!(presets(&[], &[bad], &names(&["claude"]), None).is_empty());
        assert!(presets(&prompts(&[]), &[], &[], None).is_empty(), "no harness, no presets");
    }

    #[test]
    fn a_preset_survives_its_picker_value() {
        let p = Preset {
            name: "coordinator".into(),
            agent: "claude".into(),
            role: "lead".into(),
            labels: vec!["a".into(), "b".into()],
            prompt: Some("line one\nline: two, with commas".into()),
        };
        assert_eq!(Preset::parse(&p.value()), Some(p.clone()));
        let bare = Preset { labels: vec![], prompt: None, ..p };
        assert_eq!(Preset::parse(&bare.value()), Some(bare));
        assert_eq!(Preset::parse("claude"), None, "a harness name is not a preset");
    }

    #[test]
    fn roles_parse_from_the_config_file() {
        let cfg: crate::config::Config = toml::from_str(
            "[[roles]]\nname = \"reviewer\"\nagent = \"codex\"\nprompt = \"review\"\nlabels = [\"review\"]\n",
        )
        .unwrap();
        assert_eq!(
            cfg.roles,
            [RoleDef {
                name: "reviewer".into(),
                agent: Some("codex".into()),
                prompt: Some("review".into()),
                labels: vec!["review".into()],
            }]
        );
    }
}
