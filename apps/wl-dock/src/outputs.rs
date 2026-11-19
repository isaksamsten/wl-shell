//! `wl-dock outputs`: the outputs, what a config can call them, and what
//! the config makes of each.

use std::path::Path;

use wayland_client::Proxy;
use wl_shell_config::ConfigFile;
use wl_shell_config::output::{Ident, Mode};

use crate::config::{self, Config, Position};

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

fn position(p: Position) -> &'static str {
    match p {
        Position::Bottom => "bottom",
        Position::Left => "left",
        Position::Right => "right",
    }
}

/// Two lines per output: what it is, then the `[outputs.…]` tables that
/// match it and the dock they make of it.
pub fn report(config: &Config, rows: &[Row]) -> String {
    if rows.is_empty() {
        return "no outputs\n".into();
    }
    let fallback = config
        .output
        .unmatched(rows.iter().map(|r| &r.ident))
        .then_some(0);
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

        let tables: Vec<String> = (config.outputs.matched(ident).into_iter())
            .map(|p| format!("[outputs.{:?}]", p.as_str()))
            .collect();
        let stands_in = fallback == Some(i);
        let mut result = match config.dock(ident, stands_in) {
            Some(layout) => format!("enabled, position = \"{}\"", position(layout.position)),
            None => "disabled".into(),
        };
        if let (true, Mode::Only(pattern)) = (stands_in, &config.output) {
            result.push_str(&format!(
                " (stands in: no output matches {:?})",
                pattern.as_str()
            ));
        }
        out.push_str(&format!("{:<width$}  ", ""));
        for table in tables {
            out.push_str(&table);
            out.push(' ');
        }
        out.push_str(&format!("→ {result}\n"));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

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
            [outputs."*"]
            enabled = false

            [outputs."Dell Inc. DELL U2720Q*"]
            enabled = true
            position = "left"
            "#,
        );
        let text = report(&c, &rows());
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 4, "{text}");
        assert!(lines[0].starts_with("DP-2 "), "{text}");
        assert!(
            lines[0].contains("\"Dell Inc. DELL U2720Q 8XYZ123 (DP-2)\""),
            "{text}"
        );
        assert!(
            lines[0].contains("Dell Inc. DELL U2720Q  2560x1440 @2  v4"),
            "{text}"
        );
        assert!(
            lines[1].ends_with(
                "[outputs.\"*\"] [outputs.\"Dell Inc. DELL U2720Q*\"] → enabled, position = \"left\""
            ),
            "{text}"
        );
        assert!(lines[2].starts_with("eDP-1 "), "{text}");
        assert!(lines[3].ends_with("[outputs.\"*\"] → disabled"), "{text}");
    }

    #[test]
    fn no_tables_means_enabled() {
        let text = report(&Config::default(), &rows());
        let second = text.lines().nth(1).unwrap();
        assert_eq!(second.trim(), "→ enabled, position = \"bottom\"");
    }

    #[test]
    fn a_pattern_that_matches_nothing_falls_back_to_the_first_output() {
        let c = config("output = \"HDMI-A-1\"\n");
        let text = report(&c, &rows());
        let lines: Vec<&str> = text.lines().collect();
        assert!(lines[1].contains("stands in"), "{text}");
        assert!(lines[3].ends_with("→ disabled"), "{text}");
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
