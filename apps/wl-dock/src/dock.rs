//! What the dock shows: one item per running app. Pure, unit tested; the
//! Wayland side feeds it the window list.

use wl_shell_core::toplevel::Toplevel;

/// One window of an app.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Window {
    /// The ext foreign toplevel identifier.
    pub id: String,
    pub title: String,
}

/// One icon in the dock: every window with the same app ID shares it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Item {
    pub app_id: String,
    /// In announcement order; never empty.
    pub windows: Vec<Window>,
}

/// The dock's items: the running apps, in the order their first window
/// appeared. Apps are grouped by app ID as is.
pub fn items(toplevels: &[Toplevel]) -> Vec<Item> {
    let mut items: Vec<Item> = Vec::new();
    for t in toplevels {
        let window = Window {
            id: t.identifier.clone(),
            title: t.title.clone(),
        };
        match items.iter_mut().find(|i| i.app_id == t.app_id) {
            Some(item) => item.windows.push(window),
            None => items.push(Item {
                app_id: t.app_id.clone(),
                windows: vec![window],
            }),
        }
    }
    items
}

#[cfg(test)]
mod tests {
    use super::*;

    fn toplevel(id: &str, app_id: &str) -> Toplevel {
        Toplevel {
            identifier: id.into(),
            app_id: app_id.into(),
            title: format!("{app_id} {id}"),
        }
    }

    fn summary(items: &[Item]) -> Vec<(&str, Vec<&str>)> {
        (items.iter())
            .map(|i| {
                let ids = i.windows.iter().map(|w| w.id.as_str()).collect();
                (i.app_id.as_str(), ids)
            })
            .collect()
    }

    #[test]
    fn empty() {
        assert!(items(&[]).is_empty());
    }

    #[test]
    fn grouped_in_order_of_first_window() {
        let tls = [
            toplevel("1", "term"),
            toplevel("2", "browser"),
            toplevel("3", "term"),
            toplevel("4", "chat"),
        ];
        assert_eq!(
            summary(&items(&tls)),
            [
                ("term", vec!["1", "3"]),
                ("browser", vec!["2"]),
                ("chat", vec!["4"]),
            ]
        );
    }

    #[test]
    fn empty_app_id_is_one_app() {
        let got = items(&[toplevel("1", ""), toplevel("2", "")]);
        assert_eq!(summary(&got), [("", vec!["1", "2"])]);
    }
}
