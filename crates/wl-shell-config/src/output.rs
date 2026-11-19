//! Naming outputs in a config: the same keys, patterns and per-output
//! tables in every program that lives on outputs.
//!
//! - [`Pattern`]: wherever a config names an output, in a value or a table
//!   name. Matched against an [`Ident`]: the connector name exactly, the
//!   description or `<make> <model>` exactly, or a glob over those two.
//! - [`Mode`]: the root `output` key (`"all"`, `"focused"` or a pattern),
//!   checked against what the program allows ([`Modes`]).
//! - [`Tables`]: `[outputs.<pattern>]`, one table per pattern with
//!   `enabled` and the program's own per-output keys. Every table that
//!   matches an output applies, least specific first, key by key.
//!
//! [`Ident`] is a plain copy of what `wl_shell_core::output::Ident` has, so
//! this crate doesn't depend on wayland.

use std::fmt;
use std::marker::PhantomData;

use globset::{Glob, GlobMatcher};
use serde::de::value::{MapAccessDeserializer, StringDeserializer};
use serde::de::{self, DeserializeSeed, Error as _, IntoDeserializer, MapAccess, Visitor};
use serde::{Deserialize, Deserializer};

/// What identifies an output, for matching it against the config.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Ident {
    /// The connector (`eDP-1`), if the compositor says.
    pub name: Option<String>,
    /// `<make> <model> <serial> (<connector>)` on most compositors.
    pub description: Option<String>,
    /// Empty if unknown.
    pub make: String,
    pub model: String,
}

impl Ident {
    /// `"<make> <model>"`, without the parts that are empty; empty if both
    /// are.
    pub fn make_model(&self) -> String {
        let (make, model) = (self.make.trim(), self.model.trim());
        match (make.is_empty(), model.is_empty()) {
            (true, _) => model.to_string(),
            (_, true) => make.to_string(),
            _ => format!("{make} {model}"),
        }
    }
}

/// How well a pattern matched an output; the greater, the more specific.
/// Globs rank by their number of literal characters.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Rank {
    /// A glob over the description or `<make> <model>`.
    Glob(usize),
    /// The description or `<make> <model>`, exactly.
    Exact,
    /// The connector name, exactly.
    Name,
}

/// A name for an output in a config; see the module docs.
#[derive(Clone)]
pub struct Pattern {
    text: String,
    glob: Option<GlobMatcher>,
    /// Characters of a glob that aren't wildcards.
    literals: usize,
}

impl PartialEq for Pattern {
    fn eq(&self, other: &Self) -> bool {
        self.text == other.text
    }
}

impl fmt::Debug for Pattern {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Pattern({:?})", self.text)
    }
}

impl Eq for Pattern {}

const META: &[char] = &['*', '?', '[', ']', '{', '}', '\\'];

impl Pattern {
    /// Fails if `text` is empty or a glob that doesn't parse.
    pub fn new(text: &str) -> Result<Self, String> {
        if text.trim().is_empty() {
            return Err("an output pattern can't be empty".into());
        }
        let glob = if text.contains(['*', '?', '[', '{']) {
            let glob = Glob::new(text)
                .map_err(|e| format!("invalid output pattern {text:?}: {}", e.kind()))?;
            Some(glob.compile_matcher())
        } else {
            None
        };
        let literals = text.chars().filter(|c| !META.contains(c)).count();
        Ok(Self {
            text: text.to_string(),
            glob,
            literals,
        })
    }

    pub fn as_str(&self) -> &str {
        &self.text
    }

    /// How well this matches `ident`, or None if it doesn't. In order: the
    /// connector name exactly; the description or `<make> <model>`
    /// exactly; a glob over the description, then `<make> <model>`.
    pub fn rank(&self, ident: &Ident) -> Option<Rank> {
        if ident.name.as_deref() == Some(self.text.as_str()) {
            return Some(Rank::Name);
        }
        let make_model = ident.make_model();
        if ident.description.as_deref() == Some(self.text.as_str())
            || (!make_model.is_empty() && make_model == self.text)
        {
            return Some(Rank::Exact);
        }
        let glob = self.glob.as_ref()?;
        // `*` matches every output, also one that says nothing about itself.
        let hit = ident
            .description
            .as_deref()
            .is_some_and(|d| glob.is_match(d))
            || glob.is_match(&make_model);
        hit.then_some(Rank::Glob(self.literals))
    }

    pub fn matches(&self, ident: &Ident) -> bool {
        self.rank(ident).is_some()
    }
}

impl fmt::Display for Pattern {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.text)
    }
}

impl<'de> Deserialize<'de> for Pattern {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        Pattern::new(&String::deserialize(d)?).map_err(D::Error::custom)
    }
}

/// Where a program shows, from the root `output` key.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Mode {
    /// On every enabled output.
    All,
    /// On the focused output, if enabled.
    Focused,
    /// Only on the output the pattern matches; the program's fallback
    /// while it matches none.
    Only(Pattern),
}

/// What a program allows for `output`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Modes {
    pub all: bool,
    pub focused: bool,
    pub only: bool,
}

/// Bits for [`mode`]: which values `output` allows.
pub const ALL: u8 = 1;
pub const FOCUSED: u8 = 2;
pub const ONLY: u8 = 4;

impl Modes {
    pub const fn from_bits(bits: u8) -> Self {
        Self {
            all: bits & ALL != 0,
            focused: bits & FOCUSED != 0,
            only: bits & ONLY != 0,
        }
    }

    /// `"all", "focused" or an output pattern`, of those allowed.
    fn expected(self) -> String {
        let mut parts = Vec::new();
        if self.all {
            parts.push("\"all\"");
        }
        if self.focused {
            parts.push("\"focused\"");
        }
        if self.only {
            parts.push("an output pattern");
        }
        match parts.as_slice() {
            [] => "nothing".into(),
            [one] => one.to_string(),
            [init @ .., last] => format!("{} or {last}", init.join(", ")),
        }
    }
}

impl Mode {
    /// Parse the value of `output`; a value `modes` doesn't allow is an
    /// error that says what is.
    pub fn parse(s: &str, modes: Modes) -> Result<Self, String> {
        let not_allowed = || {
            format!(
                "output = {s:?} isn't supported here: expected {}",
                modes.expected()
            )
        };
        match s {
            "all" if modes.all => Ok(Mode::All),
            "focused" if modes.focused => Ok(Mode::Focused),
            "all" | "focused" => Err(not_allowed()),
            _ if modes.only => Pattern::new(s).map(Mode::Only),
            _ => Err(not_allowed()),
        }
    }

    /// Whether the output is one this mode shows on, before `[outputs.…]`
    /// tables: `Only` only on what its pattern matches.
    pub fn matches(&self, ident: &Ident) -> bool {
        match self {
            Mode::All | Mode::Focused => true,
            Mode::Only(p) => p.matches(ident),
        }
    }

    /// Whether this is `Only` and none of `idents` matches: the program
    /// then uses its fallback output (and [`Tables::resolve_as`] with
    /// `true`).
    pub fn unmatched<'a>(&self, idents: impl IntoIterator<Item = &'a Ident>) -> bool {
        match self {
            Mode::Only(p) => !idents.into_iter().any(|i| p.matches(i)),
            _ => false,
        }
    }
}

/// For `#[serde(deserialize_with = "output::mode::<_, { output::ALL | output::ONLY }>")]`:
/// the `output` key, allowing the values in `ALLOWED` ([`ALL`],
/// [`FOCUSED`], [`ONLY`]).
pub fn mode<'de, D: Deserializer<'de>, const ALLOWED: u8>(d: D) -> Result<Mode, D::Error> {
    Mode::parse(&String::deserialize(d)?, Modes::from_bits(ALLOWED)).map_err(D::Error::custom)
}

/// A program's per-output keys, laid over one another.
pub trait Overlay {
    /// Set what `over` sets (its `Some`s), leave the rest.
    fn overlay(&mut self, over: &Self);
}

/// Overlay one `Option`: `over` wins if it is set.
pub fn overlay_option<V: Clone>(base: &mut Option<V>, over: &Option<V>) {
    if over.is_some() {
        base.clone_from(over);
    }
}

/// For programs with no per-output keys of their own: `[outputs.<p>]`
/// takes `enabled` only.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Bare {}

impl Overlay for Bare {
    fn overlay(&mut self, _: &Self) {}
}

/// One `[outputs.<pattern>]` table.
#[derive(Clone, Debug, PartialEq)]
pub struct Table<T> {
    pub enabled: Option<bool>,
    /// The program's own keys.
    pub rest: T,
}

// Not `#[serde(flatten)]`, which would let keys `T` doesn't know through
// even with `deny_unknown_fields` on `T`: `enabled` is taken out of the
// map and `T` gets the rest as it is.
impl<'de, T: Deserialize<'de>> Deserialize<'de> for Table<T> {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct V<T>(PhantomData<T>);

        impl<'de, T: Deserialize<'de>> Visitor<'de> for V<T> {
            type Value = Table<T>;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a table")
            }

            fn visit_map<A: MapAccess<'de>>(self, map: A) -> Result<Table<T>, A::Error> {
                let mut enabled = None;
                let rest = T::deserialize(MapAccessDeserializer::new(WithoutEnabled {
                    inner: map,
                    enabled: &mut enabled,
                }))?;
                Ok(Table { enabled, rest })
            }
        }

        d.deserialize_map(V(PhantomData))
    }
}

/// A map without its `enabled` key, which is kept aside.
struct WithoutEnabled<'a, A> {
    inner: A,
    enabled: &'a mut Option<bool>,
}

/// A key, or `enabled`.
enum Key<K> {
    Enabled,
    Other(K),
}

/// Reads a key: `enabled` is recognized and the seed left for the next
/// key; any other goes to the seed.
struct KeySeed<'a, S>(&'a mut Option<S>);

impl<'de, S: DeserializeSeed<'de>> DeserializeSeed<'de> for KeySeed<'_, S> {
    type Value = Key<S::Value>;

    fn deserialize<D: Deserializer<'de>>(self, d: D) -> Result<Self::Value, D::Error> {
        let key = String::deserialize(d)?;
        if key == "enabled" {
            return Ok(Key::Enabled);
        }
        let key: StringDeserializer<D::Error> = key.into_deserializer();
        let seed = self.0.take().expect("seed is kept until used");
        seed.deserialize(key).map(Key::Other)
    }
}

impl<'de, A: MapAccess<'de>> MapAccess<'de> for WithoutEnabled<'_, A> {
    type Error = A::Error;

    fn next_key_seed<K: DeserializeSeed<'de>>(
        &mut self,
        seed: K,
    ) -> Result<Option<K::Value>, Self::Error> {
        let mut seed = Some(seed);
        loop {
            match self.inner.next_key_seed(KeySeed(&mut seed))? {
                None => return Ok(None),
                Some(Key::Other(k)) => return Ok(Some(k)),
                Some(Key::Enabled) => {
                    *self.enabled = Some(self.inner.next_value::<bool>()?);
                }
            }
        }
    }

    fn next_value_seed<V: DeserializeSeed<'de>>(
        &mut self,
        seed: V,
    ) -> Result<V::Value, Self::Error> {
        self.inner.next_value_seed(seed)
    }

    fn size_hint(&self) -> Option<usize> {
        self.inner.size_hint()
    }
}

/// `[outputs.<pattern>]`, in the order of the file.
#[derive(Clone, Debug, PartialEq)]
pub struct Tables<T>(Vec<(Pattern, Table<T>)>);

impl<T> Default for Tables<T> {
    fn default() -> Self {
        Self(Vec::new())
    }
}

impl<T> Tables<T> {
    pub fn new(tables: Vec<(Pattern, Table<T>)>) -> Self {
        Self(tables)
    }

    pub fn iter(&self) -> impl Iterator<Item = &(Pattern, Table<T>)> {
        self.0.iter()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// The tables that match `ident`, least specific first; of equal
    /// specificity, the one earlier in the file first.
    fn matching(&self, ident: &Ident) -> Vec<(&Pattern, &Table<T>)> {
        let mut found: Vec<_> = self
            .0
            .iter()
            .filter_map(|(p, t)| p.rank(ident).map(|r| (r, p, t)))
            .collect();
        found.sort_by_key(|(r, ..)| *r); // stable
        found.into_iter().map(|(_, p, t)| (p, t)).collect()
    }

    /// The patterns that matched `ident`, in the order they apply.
    pub fn matched(&self, ident: &Ident) -> Vec<&Pattern> {
        self.matching(ident).into_iter().map(|(p, _)| p).collect()
    }
}

impl<T: Overlay + Default> Tables<T> {
    /// The settings for `ident`: the tables that match, folded least
    /// specific first. None if the output is disabled: by a table's
    /// `enabled = false` (the last of the matching tables that sets
    /// `enabled` decides), or by `mode` being `Only` another output.
    pub fn resolve(&self, mode: &Mode, ident: &Ident) -> Option<T> {
        self.resolve_as(mode.matches(ident), ident)
    }

    /// As [`resolve`](Self::resolve), starting from `enabled` instead of
    /// from what the mode says: `true` for a program's fallback output.
    pub fn resolve_as(&self, mut enabled: bool, ident: &Ident) -> Option<T> {
        let mut out = T::default();
        for (_, table) in self.matching(ident) {
            out.overlay(&table.rest);
            if let Some(e) = table.enabled {
                enabled = e;
            }
        }
        enabled.then_some(out)
    }
}

impl<'de, T: Deserialize<'de>> Deserialize<'de> for Tables<T> {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct V<T>(PhantomData<T>);

        impl<'de, T: Deserialize<'de>> Visitor<'de> for V<T> {
            type Value = Tables<T>;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a table of output patterns")
            }

            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Tables<T>, A::Error> {
                let mut tables = Vec::new();
                while let Some(key) = map.next_key::<String>()? {
                    let at = |e: &dyn fmt::Display| format!("[outputs.{key:?}]: {e}");
                    let pattern = Pattern::new(&key).map_err(|e| de::Error::custom(at(&e)))?;
                    let table = map
                        .next_value::<Table<T>>()
                        .map_err(|e| de::Error::custom(at(&e)))?;
                    tables.push((pattern, table));
                }
                Ok(Tables(tables))
            }
        }

        d.deserialize_map(V(PhantomData))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Clone, Debug, Default, Deserialize, PartialEq)]
    #[serde(deny_unknown_fields)]
    struct Own {
        edge: Option<String>,
        size: Option<u32>,
    }

    impl Overlay for Own {
        fn overlay(&mut self, over: &Self) {
            overlay_option(&mut self.edge, &over.edge);
            overlay_option(&mut self.size, &over.size);
        }
    }

    #[derive(Debug, Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Config {
        #[serde(
            default = "all",
            deserialize_with = "mode::<_, { ALL | FOCUSED | ONLY }>"
        )]
        output: Mode,
        #[serde(default)]
        outputs: Tables<Own>,
    }

    fn all() -> Mode {
        Mode::All
    }

    #[derive(Debug, Deserialize)]
    #[allow(dead_code)]
    struct Dockish {
        #[serde(default = "all", deserialize_with = "mode::<_, { ALL | ONLY }>")]
        output: Mode,
        #[serde(default)]
        outputs: Tables<Bare>,
    }

    fn config(s: &str) -> Config {
        toml::from_str(s).unwrap_or_else(|e| panic!("{e}"))
    }

    fn error<T: fmt::Debug + for<'de> Deserialize<'de>>(s: &str) -> String {
        toml::from_str::<T>(s).unwrap_err().to_string()
    }

    fn dell() -> Ident {
        Ident {
            name: Some("DP-2".into()),
            description: Some("Dell Inc. DELL U2720Q 8XYZ123 (DP-2)".into()),
            make: "Dell Inc.".into(),
            model: "DELL U2720Q".into(),
        }
    }

    fn boe() -> Ident {
        Ident {
            name: Some("eDP-1".into()),
            description: Some("BOE 0x0BCA Unknown (eDP-1)".into()),
            make: "BOE".into(),
            model: "0x0BCA".into(),
        }
    }

    fn pat(s: &str) -> Pattern {
        Pattern::new(s).unwrap()
    }

    fn edge(c: &Config, ident: &Ident) -> Option<Option<String>> {
        c.outputs.resolve(&c.output, ident).map(|o| o.edge)
    }

    #[test]
    fn exact_name_beats_description_beats_glob() {
        let d = dell();
        assert_eq!(pat("DP-2").rank(&d), Some(Rank::Name));
        assert_eq!(
            pat("Dell Inc. DELL U2720Q 8XYZ123 (DP-2)").rank(&d),
            Some(Rank::Exact)
        );
        assert_eq!(pat("Dell Inc. DELL U2720Q").rank(&d), Some(Rank::Exact));
        assert!(matches!(pat("*U2720Q*").rank(&d), Some(Rank::Glob(_))));
        assert!(Rank::Name > Rank::Exact);
        assert!(Rank::Exact > Rank::Glob(1000));
        assert_eq!(pat("DP-3").rank(&d), None);
        assert_eq!(pat("DP-").rank(&d), None);
        // Matching is exact, not by prefix or case.
        assert_eq!(pat("dp-2").rank(&d), None);
        assert_eq!(pat("Dell Inc. DELL U2720Q 8XYZ123").rank(&d), None);
    }

    #[test]
    fn globs_and_their_specificity() {
        let d = dell();
        let short = pat("Dell*").rank(&d).unwrap();
        let long = pat("Dell Inc. DELL U2720Q*").rank(&d).unwrap();
        assert!(long > short, "{long:?} {short:?}");
        assert_eq!(pat("*").rank(&d), Some(Rank::Glob(0)));
        // Over the description as well as make and model.
        assert!(pat("*8XYZ123*").matches(&d));
        assert!(pat("Dell Inc. DELL U27?0Q").matches(&d));
        assert!(pat("Dell Inc. DELL U27[12]0Q").matches(&d));
        assert!(!pat("Dell Inc. DELL U27[34]0Q").matches(&d));
        assert!(!pat("*8XYZ123*").matches(&boe()));
        assert!(pat("{Dell,BOE}*").matches(&boe()));
        // The connector name isn't globbed.
        assert!(!pat("*DP-2").matches(&Ident {
            name: Some("DP-2".into()),
            ..Ident::default()
        }));
    }

    #[test]
    fn outputs_that_say_little() {
        let nothing = Ident::default();
        let only_make_model = Ident {
            make: "Acme".into(),
            model: "Panel 1".into(),
            ..Ident::default()
        };
        let only_name = Ident {
            name: Some("HDMI-A-1".into()),
            ..Ident::default()
        };
        for i in [&nothing, &only_make_model, &only_name] {
            assert!(pat("*").matches(i), "{i:?}");
        }
        assert!(!pat("Acme*").matches(&nothing));
        assert!(pat("Acme Panel 1").matches(&only_make_model));
        assert!(pat("Acme*").matches(&only_make_model));
        assert_eq!(pat("Acme").rank(&only_make_model), None);
        assert!(pat("HDMI-A-1").matches(&only_name));
        assert!(!pat("Acme*").matches(&only_name));
        // Only a make, or only a model.
        let make = Ident {
            make: "Acme".into(),
            ..Ident::default()
        };
        assert_eq!(pat("Acme").rank(&make), Some(Rank::Exact));
        let c = config("[outputs.\"*\"]\nedge = \"left\"\n");
        assert_eq!(edge(&c, &nothing), Some(Some("left".into())));
    }

    #[test]
    fn bad_patterns() {
        assert!(Pattern::new("").is_err());
        assert!(Pattern::new("  ").is_err());
        assert!(Pattern::new("[").is_err());
        assert!(Pattern::new("{a").is_err());
        let e = error::<Config>("[outputs.\"[\"]\nenabled = true\n");
        assert!(e.contains("outputs") && e.contains("\"[\""), "{e}");
        let e = error::<Config>("output = \"[\"\n");
        assert!(e.contains("invalid output pattern"), "{e}");
    }

    #[test]
    fn tables_overlay_least_specific_first() {
        let c = config(
            r#"
            [outputs."Dell Inc. DELL U2720Q*"]
            edge = "left"

            [outputs."*"]
            edge = "right"
            size = 40

            [outputs.DP-2]
            size = 60
            "#,
        );
        let d = c.outputs.resolve(&c.output, &dell()).unwrap();
        assert_eq!(d.edge.as_deref(), Some("left")); // glob over "*"
        assert_eq!(d.size, Some(60)); // the name over "*"
        let b = c.outputs.resolve(&c.output, &boe()).unwrap();
        assert_eq!(b.edge.as_deref(), Some("right"));
        assert_eq!(b.size, Some(40));
        let order: Vec<_> = c
            .outputs
            .matched(&dell())
            .iter()
            .map(|p| p.as_str())
            .collect();
        assert_eq!(order, ["*", "Dell Inc. DELL U2720Q*", "DP-2"]);
        assert_eq!(c.outputs.matched(&boe()), [&pat("*")]);
        assert!(c.outputs.matched(&Ident::default()).len() == 1);
    }

    #[test]
    fn equal_specificity_goes_to_the_later_table() {
        let text = |first: &str, second: &str| {
            format!("[outputs.\"{first}\"]\nedge = \"a\"\n[outputs.\"{second}\"]\nedge = \"b\"\n")
        };
        // Both match with four literal characters.
        let c = config(&text("Dell*", "*Dell*"));
        assert_eq!(edge(&c, &dell()), Some(Some("b".into())));
        let c = config(&text("*Dell*", "Dell*"));
        assert_eq!(edge(&c, &dell()), Some(Some("b".into())));
        let c = config(&text("Dell*", "*Dell*"));
        assert_eq!(c.outputs.matched(&dell()), [&pat("Dell*"), &pat("*Dell*")]);
    }

    #[test]
    fn enabled() {
        // Enabled unless a table says otherwise.
        let c = config("[outputs.eDP-1]\nedge = \"top\"\n");
        assert!(edge(&c, &dell()).is_some());
        let c = config("[outputs.eDP-1]\nenabled = false\n");
        assert!(edge(&c, &boe()).is_none());
        assert!(edge(&c, &dell()).is_some());
        // "Only this monitor": disable "*", enable it.
        let c = config(
            r#"
            [outputs."*"]
            enabled = false

            [outputs."Dell Inc. DELL U2720Q*"]
            enabled = true
            "#,
        );
        assert!(edge(&c, &dell()).is_some());
        assert!(edge(&c, &boe()).is_none());
        // The same, with the connector changed.
        let moved = Ident {
            name: Some("DP-5".into()),
            description: Some("Dell Inc. DELL U2720Q 8XYZ123 (DP-5)".into()),
            ..dell()
        };
        assert!(edge(&c, &moved).is_some());
        // A table that only configures leaves it as it was.
        let c = config(
            r#"
            [outputs."*"]
            enabled = false

            [outputs."Dell*"]
            edge = "left"
            "#,
        );
        assert!(edge(&c, &dell()).is_none());
    }

    #[test]
    fn a_pattern_as_output_and_its_fallback() {
        let c = config("output = \"Dell Inc. DELL U2720Q*\"\n[outputs.\"*\"]\nedge = \"top\"\n");
        assert_eq!(c.output, Mode::Only(pat("Dell Inc. DELL U2720Q*")));
        assert_eq!(edge(&c, &dell()), Some(Some("top".into())));
        assert!(edge(&c, &boe()).is_none());
        assert!(!c.output.unmatched([&dell(), &boe()]));
        // Not connected: the program falls back, and the output it picks
        // is enabled all the same.
        assert!(c.output.unmatched([&boe()]));
        assert!(c.outputs.resolve_as(true, &boe()).is_some());
        // Tables can still turn on or off what the mode says.
        let c = config("output = \"DP-2\"\n[outputs.DP-2]\nenabled = false\n");
        assert!(edge(&c, &dell()).is_none());
        let c = config("output = \"DP-2\"\n[outputs.eDP-1]\nenabled = true\n");
        assert!(edge(&c, &boe()).is_some());
        // All and focused don't restrict, and have no fallback.
        assert!(!Mode::All.unmatched([&boe()]));
        assert!(!Mode::Focused.unmatched([]));
        assert!(Mode::Focused.matches(&boe()));
    }

    #[test]
    fn modes_a_program_does_not_allow() {
        let c = config("output = \"focused\"\n");
        assert_eq!(c.output, Mode::Focused);
        assert_eq!(config("output = \"all\"\n").output, Mode::All);
        assert_eq!(config("").output, Mode::All);

        let e = error::<Dockish>("output = \"focused\"\n");
        assert!(
            e.contains("focused")
                && e.contains("isn't supported")
                && e.contains("\"all\" or an output pattern"),
            "{e}"
        );
        assert!(toml::from_str::<Dockish>("output = \"DP-1\"\n").is_ok());

        let only_all_focused = Modes {
            all: true,
            focused: true,
            only: false,
        };
        assert_eq!(Mode::parse("focused", only_all_focused), Ok(Mode::Focused));
        let e = Mode::parse("DP-1", only_all_focused).unwrap_err();
        assert!(e.contains("\"all\" or \"focused\""), "{e}");
        let none = Modes::from_bits(0);
        assert!(Mode::parse("all", none).is_err());
        let e = Mode::parse("x", Modes::from_bits(ALL | FOCUSED | ONLY)).unwrap();
        assert_eq!(e, Mode::Only(pat("x")));
        let e = Mode::parse("", Modes::from_bits(ALL | ONLY)).unwrap_err();
        assert!(e.contains("empty"), "{e}");
    }

    #[test]
    fn unknown_per_output_keys() {
        let e = error::<Config>("[outputs.\"*\"]\ncolour = \"red\"\n");
        assert!(
            e.contains("outputs") && e.contains("\"*\"") && e.contains("colour"),
            "{e}"
        );
        // A program with no keys of its own takes `enabled` only.
        assert!(toml::from_str::<Dockish>("[outputs.DP-1]\nenabled = false\n").is_ok());
        let e = error::<Dockish>("[outputs.DP-1]\nedge = \"left\"\n");
        assert!(e.contains("DP-1") && e.contains("edge"), "{e}");
        // Wrong types are named too.
        let e = error::<Config>("[outputs.DP-1]\nenabled = \"yes\"\n");
        assert!(e.contains("DP-1"), "{e}");
        let e = error::<Config>("[outputs.DP-1]\nsize = \"big\"\n");
        assert!(e.contains("DP-1"), "{e}");
        let e = error::<Config>("outputs = 3\n");
        assert!(e.contains("table"), "{e}");
    }

    #[test]
    fn no_tables_is_all_enabled() {
        let c = config("");
        assert!(c.outputs.is_empty());
        assert_eq!(edge(&c, &boe()), Some(None));
        assert!(c.outputs.matched(&boe()).is_empty());
    }
}
