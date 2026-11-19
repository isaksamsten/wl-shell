//! Which outputs get a column (`pick`), and `wl-note outputs`: the outputs,
//! what a config can call them, and what the config makes of each.

use std::path::Path;

use wayland_client::Proxy;
use wl_shell_config::ConfigFile;
use wl_shell_config::output::{Ident, Mode};

use crate::config::{self, Config, Edge, NoteLayer};

/// How `output` chooses among the enabled outputs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Rule {
    /// Every enabled output.
    All,
    /// One: where the context is, else where the column is.
    Focused,
    /// One: the first enabled output, standing in for a pattern that no
    /// output matches.
    First,
}

/// The rule for `mode`; `unmatched` is true if it names a pattern that
/// matches none of the outputs (`Mode::unmatched`). A pattern that does
/// match shows on every enabled output it matches, which `Config::column`
/// has already settled.
pub fn rule(mode: &Mode, unmatched: bool) -> Rule {
    match mode {
        Mode::All => Rule::All,
        Mode::Focused => Rule::Focused,
        Mode::Only(_) if unmatched => Rule::First,
        Mode::Only(_) => Rule::All,
    }
}

/// Where things are, as indices into the outputs.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Whereabouts {
    /// The output of a column that keeps its place: a note is open in it.
    pub held: Option<usize>,
    /// The output the context's window is on, if the stream said.
    pub context: Option<usize>,
    /// The output of the column now shown.
    pub current: Option<usize>,
}

/// The outputs (indices into `enabled`) that get a column. A disabled
/// output never does: `focused` keeps the column on the enabled output it
/// is on when the context is on a disabled one, and falls back to the
/// first enabled output. No enabled output: no column.
pub fn pick(rule: Rule, enabled: &[bool], at: Whereabouts) -> Vec<usize> {
    let ok = |i: Option<usize>| i.filter(|&i| enabled.get(i).copied().unwrap_or(false));
    let first = enabled.iter().position(|&e| e);
    match rule {
        Rule::All => (0..enabled.len()).filter(|&i| enabled[i]).collect(),
        Rule::First => first.into_iter().collect(),
        Rule::Focused => ok(at.held)
            .or_else(|| ok(at.context))
            .or_else(|| ok(at.current))
            .or(first)
            .into_iter()
            .collect(),
    }
}

/// One output, as listed.
pub struct Row {
    pub key: String,
    pub ident: Ident,
    pub scale: i32,
    pub size: Option<(i32, i32)>,
    /// The `wl_output` version.
    pub version: u32,
}

/// Connect, list the outputs and describe them against the config.
pub fn run(explicit: Option<&Path>) -> Result<String, String> {
    let config = Config::read(explicit).map_err(|e| format!("config: {e}"))?;
    let outputs = wl_shell_core::output::list().map_err(|e| e.to_string())?;
    let rows: Vec<Row> = outputs
        .iter()
        .map(|o| Row {
            key: o.key.clone(),
            ident: config::ident(&o.ident),
            scale: o.scale,
            size: o.logical_size,
            version: o.wl.version(),
        })
        .collect();
    Ok(report(&config, &rows))
}

fn edge(e: Edge) -> &'static str {
    match e {
        Edge::Left => "left",
        Edge::Right => "right",
    }
}

fn layer(l: NoteLayer) -> &'static str {
    match l {
        NoteLayer::Top => "top",
        NoteLayer::Overlay => "overlay",
    }
}

/// Two lines per output: what it is, then the `[outputs.…]` tables that
/// match it and the column they make of it.
pub fn report(config: &Config, rows: &[Row]) -> String {
    if rows.is_empty() {
        return "no outputs\n".into();
    }
    let unmatched = config.output.unmatched(rows.iter().map(|r| &r.ident));
    let enabled: Vec<bool> = rows
        .iter()
        .map(|r| config.column(&r.ident, unmatched).is_some())
        .collect();
    let rule = rule(&config.output, unmatched);
    let used = pick(rule, &enabled, Whereabouts::default());
    let width = rows
        .iter()
        .map(|r| r.key.chars().count())
        .max()
        .unwrap_or(0);
    let mut out = String::new();
    for (i, row) in rows.iter().enumerate() {
        let ident = &row.ident;
        let description = match &ident.description {
            Some(d) => format!("\"{d}\""),
            None => "-".into(),
        };
        let make_model = match ident.make_model() {
            m if m.is_empty() => "-".into(),
            m => m,
        };
        let size = match row.size {
            Some((w, h)) => format!("{w}x{h}"),
            None => "-".into(),
        };
        out.push_str(&format!(
            "{:<width$}  {description}  {make_model}  {size} @{}  v{}\n",
            row.key, row.scale, row.version
        ));

        out.push_str(&format!("{:<width$}  ", ""));
        for pattern in config.outputs.matched(ident) {
            out.push_str(&format!("[outputs.{:?}] ", pattern.as_str()));
        }
        let result = match config.column(ident, unmatched) {
            None => "disabled".to_string(),
            Some(layout) => {
                let base = format!(
                    "enabled, edge = \"{}\", layer = \"{}\"",
                    edge(layout.edge),
                    layer(layout.layer)
                );
                match (&config.output, used.contains(&i)) {
                    (Mode::Focused, _) => format!("{base} (when focused)"),
                    (_, false) => "enabled, not used (another output stands in)".into(),
                    (Mode::Only(pattern), true) if unmatched => format!(
                        "{base} (stands in: no output matches {:?})",
                        pattern.as_str()
                    ),
                    _ => base,
                }
            }
        };
        out.push_str(&format!("→ {result}\n"));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(held: Option<usize>, context: Option<usize>, current: Option<usize>) -> Whereabouts {
        Whereabouts {
            held,
            context,
            current,
        }
    }

    #[test]
    fn focused_follows_the_context() {
        let on = [true, true, true];
        let none = at(None, None, None);
        // Nothing known: the first.
        assert_eq!(pick(Rule::Focused, &on, none), [0]);
        // The context's output.
        assert_eq!(pick(Rule::Focused, &on, at(None, Some(2), Some(0))), [2]);
        // No stream, or not placed yet: stay.
        assert_eq!(pick(Rule::Focused, &on, at(None, None, Some(1))), [1]);
        // An open note holds the column.
        assert_eq!(pick(Rule::Focused, &on, at(Some(1), Some(2), Some(1))), [1]);
    }

    #[test]
    fn focused_never_goes_to_a_disabled_output() {
        let enabled = [false, true, true];
        // The context is on the disabled one: the column stays.
        assert_eq!(
            pick(Rule::Focused, &enabled, at(None, Some(0), Some(2))),
            [2]
        );
        // Nothing to stay on: the first enabled.
        assert_eq!(pick(Rule::Focused, &enabled, at(None, Some(0), None)), [1]);
        // The output it was on got disabled.
        assert_eq!(pick(Rule::Focused, &enabled, at(None, None, Some(0))), [1]);
        // A held output that got disabled doesn't hold.
        assert_eq!(
            pick(Rule::Focused, &enabled, at(Some(0), Some(2), Some(0))),
            [2]
        );
        // An index that is gone is skipped.
        assert_eq!(
            pick(Rule::Focused, &enabled, at(None, Some(7), Some(2))),
            [2]
        );
    }

    #[test]
    fn all_skips_the_disabled() {
        assert_eq!(
            pick(Rule::All, &[true, false, true], at(None, None, None)),
            [0, 2]
        );
        assert!(pick(Rule::All, &[], at(None, None, None)).is_empty());
    }

    #[test]
    fn first_is_the_first_enabled() {
        assert_eq!(
            pick(Rule::First, &[false, true, true], at(None, Some(2), None)),
            [1]
        );
    }

    #[test]
    fn no_enabled_output_no_column() {
        let off = [false, false];
        for rule in [Rule::All, Rule::Focused, Rule::First] {
            assert!(pick(rule, &off, at(Some(0), Some(1), Some(0))).is_empty());
        }
    }

    #[test]
    fn rules_from_the_mode() {
        let only = Mode::parse(
            "DP-1",
            wl_shell_config::output::Modes::from_bits(wl_shell_config::output::ONLY),
        )
        .unwrap();
        assert_eq!(rule(&Mode::All, false), Rule::All);
        assert_eq!(rule(&Mode::Focused, false), Rule::Focused);
        assert_eq!(rule(&only, false), Rule::All);
        assert_eq!(rule(&only, true), Rule::First);
    }

    fn row(key: &str, description: &str, make: &str, model: &str) -> Row {
        Row {
            key: key.into(),
            ident: Ident {
                name: Some(key.into()),
                description: Some(description.into()),
                make: make.into(),
                model: model.into(),
            },
            scale: 2,
            size: Some((2560, 1440)),
            version: 4,
        }
    }

    fn rows() -> Vec<Row> {
        vec![
            row(
                "DP-2",
                "Dell Inc. DELL U2720Q 8XYZ123 (DP-2)",
                "Dell Inc.",
                "DELL U2720Q",
            ),
            row("eDP-1", "BOE 0x0BCA Unknown (eDP-1)", "BOE", "0x0BCA"),
        ]
    }

    fn config(s: &str) -> Config {
        <Config as ConfigFile>::parse(s).unwrap()
    }

    #[test]
    fn lists_outputs_and_what_the_config_makes_of_them() {
        let c = config(
            r#"
            output = "all"

            [outputs."*"]
            enabled = false

            [outputs."Dell Inc. DELL U2720Q*"]
            enabled = true
            edge = "left"
            "#,
        );
        let text = report(&c, &rows());
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 4, "{text}");
        assert!(lines[0].starts_with("DP-2 "), "{text}");
        assert!(
            lines[0].contains("Dell Inc. DELL U2720Q  2560x1440 @2  v4"),
            "{text}"
        );
        assert!(
            lines[1].ends_with(
                "[outputs.\"*\"] [outputs.\"Dell Inc. DELL U2720Q*\"] → enabled, edge = \"left\", layer = \"top\""
            ),
            "{text}"
        );
        assert!(lines[2].starts_with("eDP-1 "), "{text}");
        assert!(lines[3].ends_with("[outputs.\"*\"] → disabled"), "{text}");
    }

    #[test]
    fn focused_says_when_focused() {
        let text = report(&Config::default(), &rows());
        let second = text.lines().nth(1).unwrap();
        assert_eq!(
            second.trim(),
            "→ enabled, edge = \"right\", layer = \"top\" (when focused)"
        );
    }

    #[test]
    fn a_pattern_that_matches_nothing_falls_back_to_the_first_enabled() {
        let c = config("output = \"HDMI-A-1\"\n");
        let text = report(&c, &rows());
        let lines: Vec<&str> = text.lines().collect();
        assert!(lines[1].contains("stands in"), "{text}");
        assert!(lines[3].contains("not used"), "{text}");
        // Connected: only that output.
        let c = config("output = \"eDP-1\"\n");
        let text = report(&c, &rows());
        let lines: Vec<&str> = text.lines().collect();
        assert!(lines[1].ends_with("→ disabled"), "{text}");
        assert!(
            lines[3].contains("enabled") && !lines[3].contains("stands in"),
            "{text}"
        );
    }

    #[test]
    fn outputs_that_say_little() {
        let nothing = Row {
            key: "output-7".into(),
            ident: Ident::default(),
            scale: 1,
            size: None,
            version: 3,
        };
        let text = report(&Config::default(), &[nothing]);
        assert!(text.starts_with("output-7  -  -  - @1  v3\n"), "{text}");
        assert_eq!(report(&Config::default(), &[]), "no outputs\n");
    }
}
