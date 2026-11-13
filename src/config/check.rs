//! The daemon's checker: what the config returned, read with the daemon's
//! own rules (#93 decisions 1, 2, 5, 8–13, 23 and 24). Malformed is an
//! error, unknown a warning, out of range a warning and clamped. Paths are
//! rooted at `provider()`; problems about the file's own table start with
//! the file's name.

use std::fmt::Write as _;
use std::time::Duration;

use super::tree::Tree;
use super::wire::{Answer, Called, Ran};
use crate::cli::PANE_PROVIDER_PROTOCOLS;
use crate::name::NAME;
use crate::placeholder::{is_escaped, whole_placeholder};

/// The most problems collected.
pub const MAX_PROBLEMS: usize = 20;
/// The longest timeout; longer ones are cut to it.
pub const MAX_TIMEOUT: Duration = Duration::from_secs(60);
/// The attach timeout when the table gives none.
pub const ATTACH_TIMEOUT: Duration = Duration::from_secs(5);

/// What the config chose.
#[derive(Debug, Clone, PartialEq)]
pub enum Choice {
    /// No pane provider: `nil` or `false`.
    None,
    /// A built-in, by name.
    Builtin(Builtin),
    /// A pane provider table used whole.
    Whole(Provider),
    /// A terminal provider and a layout provider, or one alone.
    Pair(Pair),
}

/// The only key of the table the config file returns.
pub const PROVIDER_KEY: &str = "provider";

/// A key of a pair (#93 decision 12).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PairKey {
    Name,
    Terminal,
    Layout,
}

impl PairKey {
    pub const ALL: [PairKey; 3] = [PairKey::Name, PairKey::Terminal, PairKey::Layout];
    /// The keys that make a table a pair, or one side alone.
    pub const SIDES: [PairKey; 2] = [PairKey::Terminal, PairKey::Layout];

    pub fn name(self) -> &'static str {
        match self {
            PairKey::Name => "name",
            PairKey::Terminal => "terminal",
            PairKey::Layout => "layout",
        }
    }

    pub fn from_name(name: &[u8]) -> Option<PairKey> {
        PairKey::ALL
            .into_iter()
            .find(|k| k.name().as_bytes() == name)
    }
}

/// The built-in pane providers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Builtin {
    Rex,
}

impl Builtin {
    pub const ALL: [Builtin; 1] = [Builtin::Rex];

    pub fn from_name(name: &[u8]) -> Option<Builtin> {
        Builtin::ALL
            .into_iter()
            .find(|b| b.name().as_bytes() == name)
    }

    pub fn name(self) -> &'static str {
        match self {
            Builtin::Rex => "rex",
        }
    }
}

/// `{ terminal = T, layout = L }`, or one side alone.
#[derive(Debug, Clone, PartialEq)]
pub struct Pair {
    /// Shown, never matched: its own `name`, else its sides' as `T+L`.
    pub name: String,
    /// Gives `open`, `close` and the attach timeout.
    pub terminal: Option<Provider>,
    /// Gives the layout capabilities, context variables and `find`'s
    /// variables.
    pub layout: Option<Provider>,
}

/// A pane provider table, with its defaults filled in.
#[derive(Debug, Clone, PartialEq)]
pub struct Provider {
    /// Shown, never matched.
    pub name: String,
    /// Where it is in what `provider()` returned, such as
    /// `provider().layout`.
    pub path: String,
    /// The capabilities it gives, in [`Capability::ALL`]'s order.
    pub capabilities: Vec<(Capability, Command)>,
    pub attach_timeout: Duration,
    /// The protocol number agreed: the highest both it and the daemon speak.
    pub protocol: i64,
    pub context_vars: Vec<String>,
    pub find_vars: Vec<String>,
}

impl Provider {
    pub fn capability(&self, c: Capability) -> Option<&Command> {
        self.capabilities
            .iter()
            .find_map(|(have, cmd)| (*have == c).then_some(cmd))
    }
}

/// A pane provider table's keys besides its capabilities (#93 decision 9).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Setting {
    Name,
    Timeouts,
    AttachTimeout,
    Protocols,
    ContextVars,
    FindVars,
}

impl Setting {
    pub const ALL: [Setting; 6] = [
        Setting::Name,
        Setting::Timeouts,
        Setting::AttachTimeout,
        Setting::Protocols,
        Setting::ContextVars,
        Setting::FindVars,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Setting::Name => "name",
            Setting::Timeouts => "timeouts",
            Setting::AttachTimeout => "attach_timeout",
            Setting::Protocols => "protocols",
            Setting::ContextVars => "context_vars",
            Setting::FindVars => "find_vars",
        }
    }

    pub fn from_name(name: &[u8]) -> Option<Setting> {
        Setting::ALL
            .into_iter()
            .find(|s| s.name().as_bytes() == name)
    }
}

/// What a capability runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Command {
    /// As written: placeholders are replaced when it runs.
    pub argv: Vec<Vec<u8>>,
    /// `None` for `events`, which runs until it ends.
    pub timeout: Option<Duration>,
}

/// Something a pane provider can do (ADR 0006).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Capability {
    Open,
    Close,
    Find,
    PlaceBeside,
    Resize,
    Focus,
    FocusDirection,
    ReportLayout,
    Events,
    OpenBeside,
    OpenFloat,
    OpenTabPage,
}

impl Capability {
    pub const ALL: [Capability; 12] = [
        Capability::Open,
        Capability::Close,
        Capability::Find,
        Capability::PlaceBeside,
        Capability::Resize,
        Capability::Focus,
        Capability::FocusDirection,
        Capability::ReportLayout,
        Capability::Events,
        Capability::OpenBeside,
        Capability::OpenFloat,
        Capability::OpenTabPage,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Capability::Open => "open",
            Capability::Close => "close",
            Capability::Find => "find",
            Capability::PlaceBeside => "place_beside",
            Capability::Resize => "resize",
            Capability::Focus => "focus",
            Capability::FocusDirection => "focus_direction",
            Capability::ReportLayout => "report_layout",
            Capability::Events => "events",
            Capability::OpenBeside => "open_beside",
            Capability::OpenFloat => "open_float",
            Capability::OpenTabPage => "open_tab_page",
        }
    }

    pub fn from_name(name: &[u8]) -> Option<Capability> {
        Capability::ALL
            .into_iter()
            .find(|c| c.name().as_bytes() == name)
    }

    /// What a terminal provider gives.
    pub fn is_terminal(self) -> bool {
        matches!(self, Capability::Open | Capability::Close)
    }

    /// What only a table returned whole gives.
    pub fn is_one_step(self) -> bool {
        matches!(
            self,
            Capability::OpenBeside | Capability::OpenFloat | Capability::OpenTabPage
        )
    }

    /// What a layout provider gives.
    pub fn is_layout(self) -> bool {
        !self.is_terminal() && !self.is_one_step()
    }

    /// Its timeout when the table gives none; `events` has none.
    pub fn default_timeout(self) -> Option<Duration> {
        let s = match self {
            Capability::Open
            | Capability::OpenBeside
            | Capability::OpenFloat
            | Capability::OpenTabPage => 5,
            Capability::Find => 3,
            Capability::Close
            | Capability::PlaceBeside
            | Capability::Resize
            | Capability::Focus
            | Capability::FocusDirection => 2,
            Capability::ReportLayout => 1,
            Capability::Events => return None,
        };
        Some(Duration::from_secs(s))
    }

    /// The placeholders it is given (#93 decision 14), besides
    /// [`EVERY_CAPABILITY_GETS`].
    pub fn placeholders(self) -> &'static [&'static str] {
        match self {
            Capability::Open => &["attach", "attach_json", "title", "pane", "cols", "rows"],
            Capability::OpenBeside => &[
                "attach",
                "attach_json",
                "title",
                "pane",
                "origin",
                "direction",
                "size",
                "ratio",
                "cols",
                "rows",
            ],
            Capability::OpenFloat => &[
                "attach",
                "attach_json",
                "title",
                "pane",
                "origin",
                "x",
                "y",
                "cols",
                "rows",
                "origin_cols",
                "origin_rows",
                "area_bounds",
            ],
            Capability::OpenTabPage => &[
                "attach",
                "attach_json",
                "title",
                "pane",
                "origin",
                "cols",
                "rows",
            ],
            Capability::Close | Capability::Focus => &["terminal"],
            Capability::Find => &["pane", "pid", "tty", "title"],
            Capability::PlaceBeside | Capability::Resize => {
                &["terminal", "origin", "direction", "size", "ratio"]
            }
            Capability::FocusDirection => &["direction", "count"],
            Capability::ReportLayout | Capability::Events => &[],
        }
    }

    fn gets(self, placeholder: &str) -> bool {
        EVERY_CAPABILITY_GETS.contains(&placeholder) || self.placeholders().contains(&placeholder)
    }
}

/// The placeholders every capability is given: `{capability}`,
/// `{protocol}` and `{exe}`.
pub const EVERY_CAPABILITY_GETS: [&str; 3] = ["capability", "protocol", "exe"];

/// How bad a problem is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    /// No pane provider for this choice.
    Error,
    /// Ignored or clamped.
    Warning,
    /// A warning only `provider check` shows.
    CheckOnly,
}

/// One thing wrong with the config, with its place.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Problem {
    pub severity: Severity,
    pub text: String,
}

/// The problems found, at most [`MAX_PROBLEMS`], in the order found.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Problems(pub Vec<Problem>);

impl Problems {
    /// Adds a problem while there is room. An error always goes in, in
    /// place of the last warning if there is no room, so the cap never
    /// turns a malformed config into an accepted one.
    fn push(&mut self, severity: Severity, text: String) {
        if self.0.len() >= MAX_PROBLEMS {
            if severity != Severity::Error {
                return;
            }
            if let Some(at) = self.0.iter().rposition(|p| p.severity != Severity::Error) {
                self.0.remove(at);
            }
        }
        self.0.push(Problem { severity, text });
    }

    pub fn has_error(&self) -> bool {
        self.0.iter().any(|p| p.severity == Severity::Error)
    }

    /// The errors' texts.
    pub fn errors(&self) -> Vec<&str> {
        self.of(Severity::Error)
    }

    /// The warnings' texts, check-only ones aside.
    pub fn warnings(&self) -> Vec<&str> {
        self.of(Severity::Warning)
    }

    fn of(&self, severity: Severity) -> Vec<&str> {
        self.0
            .iter()
            .filter(|p| p.severity == severity)
            .map(|p| p.text.as_str())
            .collect()
    }

    /// The Neovim message (#93 decision 20): `<name>: config: <first
    /// error, else first warning> (and N more: <name> provider check)`.
    /// None without a problem to show.
    pub fn message(&self) -> Option<String> {
        let shown: Vec<&Problem> = self
            .0
            .iter()
            .filter(|p| p.severity != Severity::CheckOnly)
            .collect();
        let first = shown
            .iter()
            .find(|p| p.severity == Severity::Error)
            .or(shown.first())?;
        let mut m = format!("{NAME}: config: {}", first.text);
        if shown.len() > 1 {
            let _ = write!(m, " (and {} more: {NAME} provider check)", shown.len() - 1);
        }
        Some(m)
    }
}

/// A choice the config made, and what was wrong but not fatal.
#[derive(Debug, Clone, PartialEq)]
pub struct Chosen {
    pub choice: Choice,
    pub warnings: Problems,
}

/// Checks what the config file named `file` (such as `init.lua`) answered.
pub fn check(answer: &Answer, file: &str) -> Result<Chosen, Problems> {
    let mut c = Checker::default();
    let choice = c.answer(&answer.ran, file);
    if c.problems.has_error() {
        return Err(c.problems);
    }
    Ok(Chosen {
        choice: choice.unwrap_or(Choice::None),
        warnings: c.problems,
    })
}

/// A problem with no place in the tree: the file failed, or its child gave
/// no answer.
pub fn failed(text: String) -> Problems {
    let mut p = Problems::default();
    p.push(Severity::Error, text);
    p
}

#[derive(Default)]
struct Checker {
    problems: Problems,
}

/// What a table is in the choice.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Role {
    Whole,
    Terminal,
    Layout,
}

const ROOT: &str = "provider()";

impl Checker {
    fn error(&mut self, text: String) {
        self.problems.push(Severity::Error, text);
    }

    fn warn(&mut self, text: String) {
        self.problems.push(Severity::Warning, text);
    }

    fn answer(&mut self, ran: &Ran, file: &str) -> Option<Choice> {
        match ran {
            Ran::CantRead(e) => self.error(format!("can't read {file}: {e}")),
            Ran::LoadFailed(e) => self.error(format!("{file} failed to load: {e}")),
            Ran::Failed(e) => self.error(format!("{file} failed: {e}")),
            Ran::FileOver(limit) => self.error(format!("{file} returned a table of {limit}")),
            Ran::Returned {
                file: top,
                provider,
            } => {
                if self.file_table(top, file) {
                    return self.called(provider);
                }
            }
        }
        None
    }

    /// The file's own table: true if it has a `provider` function.
    fn file_table(&mut self, top: &Tree, file: &str) -> bool {
        let Tree::Table(pairs) = top else {
            self.error(format!(
                "{file} returned {}, not a table with provider",
                kind(top)
            ));
            return false;
        };
        let mut found = false;
        let mut has_provider = false;
        for (k, v) in pairs {
            if !is_key(k, PROVIDER_KEY) {
                self.warn(format!(
                    "{file} returned an unknown key, {}; only provider is read",
                    key_shown(k)
                ));
                continue;
            }
            match v {
                Tree::Bool(false) => {}
                Tree::Marker(super::tree::Marker::Function) => {
                    has_provider = true;
                    found = true;
                }
                other => {
                    has_provider = true;
                    self.error(format!(
                        "{file} returned provider as {}, not a function",
                        kind(other)
                    ));
                }
            }
        }
        if !has_provider {
            self.error(format!("{file} returned a table without provider"));
        }
        found
    }

    fn called(&mut self, called: &Called) -> Option<Choice> {
        let tree = match called {
            Called::Returned(tree) => tree,
            Called::Raised(e) => {
                self.error(format!("{ROOT} raised: {e}"));
                return None;
            }
            Called::Over(limit) => {
                self.error(format!("{ROOT} returned {limit}"));
                return None;
            }
            Called::NotCalled => {
                self.error(format!("{ROOT} wasn't called"));
                return None;
            }
        };
        match tree {
            Tree::Nil | Tree::Bool(false) => Some(Choice::None),
            Tree::String(s) => match Builtin::from_name(s) {
                Some(builtin) => Some(Choice::Builtin(builtin)),
                None => {
                    self.error(format!(
                        "{ROOT} returned {}; the only built-in name is \"rex\"",
                        quoted(s)
                    ));
                    None
                }
            },
            Tree::Marker(super::tree::Marker::Function) => {
                self.error(format!(
                    "{ROOT} returned a function (call it: return f(attach), not f)"
                ));
                None
            }
            Tree::Table(_) if is_pair(tree) => self.pair(tree),
            Tree::Table(_) => self.provider(tree, ROOT, Role::Whole).map(Choice::Whole),
            other => {
                self.error(format!(
                    "{ROOT} returned {}; expected nil, false, \"rex\" or a table",
                    a(other)
                ));
                None
            }
        }
    }

    fn pair(&mut self, tree: &Tree) -> Option<Choice> {
        let Tree::Table(pairs) = tree else {
            return None;
        };
        let mut name = None;
        let mut ok = true;
        let mut sides = [None, None];
        for (k, v) in pairs {
            if matches!(v, Tree::Bool(false)) {
                continue;
            }
            let path = format!("{ROOT}{}", segment(k));
            let key = key_bytes(k).unwrap_or(b"");
            match PairKey::from_name(key) {
                Some(side @ (PairKey::Terminal | PairKey::Layout)) => {
                    let (i, role) = if side == PairKey::Terminal {
                        (0, Role::Terminal)
                    } else {
                        (1, Role::Layout)
                    };
                    match v {
                        Tree::String(s) if Builtin::from_name(s) == Some(Builtin::Rex) => {
                            self.error(format!("{path}: rex is never half of a pair"));
                            ok = false;
                        }
                        Tree::Table(_) if is_pair(v) => {
                            self.error(format!("{path}: a pair can't be half of a pair"));
                            ok = false;
                        }
                        Tree::Table(_) => match self.provider(v, &path, role) {
                            Some(p) => sides[i] = Some(p),
                            None => ok = false,
                        },
                        other => {
                            self.error(format!(
                                "{path}: expected a pane provider table, got {}",
                                other.type_name()
                            ));
                            ok = false;
                        }
                    }
                }
                Some(PairKey::Name) => match self.name(v, &path) {
                    Some(n) => name = Some(n),
                    None => ok = false,
                },
                None if Capability::from_name(key).is_some() => {
                    self.error(format!(
                        "{path}: a capability can't sit beside terminal and layout; move it \
                         into one of them"
                    ));
                    ok = false;
                }
                None => self.warn(format!("{path}: unknown field; ignored")),
            }
        }
        if !ok {
            return None;
        }
        let [terminal, layout] = sides;
        let name = name.unwrap_or_else(|| {
            [&terminal, &layout]
                .into_iter()
                .flatten()
                .map(|p| p.name.as_str())
                .collect::<Vec<_>>()
                .join("+")
        });
        Some(Choice::Pair(Pair {
            name,
            terminal,
            layout,
        }))
    }

    /// A pane provider table at `path`, used as `role`.
    fn provider(&mut self, tree: &Tree, path: &str, role: Role) -> Option<Provider> {
        let Tree::Table(pairs) = tree else {
            return None;
        };
        let errors_before = self.problems.errors().len();
        let mut name = None;
        let mut timeouts: Vec<(Capability, Duration)> = Vec::new();
        let mut attach_timeout = ATTACH_TIMEOUT;
        let mut protocols: Vec<i64> = vec![1];
        let mut context_vars = Vec::new();
        let mut find_vars = Vec::new();
        let mut argvs: Vec<(Capability, Vec<Vec<u8>>)> = Vec::new();
        for (k, v) in pairs {
            if matches!(v, Tree::Bool(false)) {
                continue;
            }
            let at = format!("{path}{}", segment(k));
            let key = key_bytes(k).unwrap_or(b"");
            if let Some(cap) = Capability::from_name(key) {
                let used = match role {
                    Role::Whole => true,
                    Role::Terminal => cap.is_terminal(),
                    Role::Layout => cap.is_layout(),
                };
                if !used {
                    if cap.is_one_step() {
                        self.problems.push(
                            Severity::CheckOnly,
                            format!(
                                "{at}: a half's one-step capability is ignored; only a table \
                                 returned whole gets one"
                            ),
                        );
                    }
                    continue;
                }
                if let Some(argv) = self.argv(v, &at)
                    && self.placeholders(cap, &argv, &at)
                {
                    argvs.push((cap, argv));
                }
                continue;
            }
            match Setting::from_name(key) {
                Some(Setting::Name) => name = self.name(v, &at),
                Some(Setting::Timeouts) => timeouts = self.timeouts(v, &at),
                Some(Setting::AttachTimeout) => {
                    if let Some(t) = self.seconds(v, &at) {
                        attach_timeout = t;
                    }
                }
                Some(Setting::Protocols) => {
                    if let Some(p) = self.protocols(v, &at) {
                        protocols = p;
                    }
                }
                Some(Setting::ContextVars) => context_vars = self.vars(v, &at),
                Some(Setting::FindVars) => find_vars = self.vars(v, &at),
                None => self.warn(format!("{at}: unknown field; ignored")),
            }
        }
        if self.problems.errors().len() > errors_before {
            return None;
        }
        if argvs.is_empty() {
            let uses = match role {
                Role::Whole => "",
                Role::Terminal => " (a terminal provider uses open and close)",
                Role::Layout => {
                    " (a layout provider uses find, place_beside, resize, focus, \
                                 focus_direction, report_layout and events)"
                }
            };
            self.error(format!("{path}: has no usable capability{uses}"));
            return None;
        }
        let Some(protocol) = agree(&protocols) else {
            let says: Vec<String> = protocols.iter().map(i64::to_string).collect();
            let speaks: Vec<String> = PANE_PROVIDER_PROTOCOLS.iter().map(i64::to_string).collect();
            self.error(format!(
                "{path}: shares no protocol number with {NAME} (it speaks {}; this table says {})",
                speaks.join(", "),
                says.join(", ")
            ));
            return None;
        };
        argvs.sort_by_key(|(c, _)| *c);
        let capabilities = argvs
            .into_iter()
            .map(|(cap, argv)| {
                let timeout = cap.default_timeout().map(|default| {
                    timeouts
                        .iter()
                        .find_map(|(c, t)| (*c == cap).then_some(*t))
                        .unwrap_or(default)
                });
                (cap, Command { argv, timeout })
            })
            .collect();
        let name = name.unwrap_or_else(|| {
            match role {
                Role::Whole => "config",
                Role::Terminal => "terminal",
                Role::Layout => "layout",
            }
            .to_string()
        });
        Some(Provider {
            name,
            path: path.to_string(),
            capabilities,
            attach_timeout,
            protocol,
            context_vars,
            find_vars,
        })
    }

    /// A list's values in order, or None after reporting why it isn't one.
    fn list<'t>(&mut self, tree: &'t Tree, path: &str, of: &str) -> Option<Vec<&'t Tree>> {
        let Tree::Table(pairs) = tree else {
            self.error(format!("{path}: expected {of}, got {}", tree.type_name()));
            return None;
        };
        let mut items: Vec<(usize, &Tree)> = Vec::new();
        for (k, v) in pairs {
            match k {
                Tree::Number(n) if *n >= 1.0 && n.fract() == 0.0 && *n <= 1e9 => {
                    items.push((*n as usize, v))
                }
                other => {
                    self.error(format!(
                        "{path}: expected {of}, but it has the key {}",
                        key_shown(other)
                    ));
                    return None;
                }
            }
        }
        items.sort_by_key(|(i, _)| *i);
        for (want, (i, _)) in items.iter().enumerate() {
            if *i != want + 1 {
                self.error(format!(
                    "{path}: nothing at [{}] but more after it (is a variable nil?)",
                    want + 1
                ));
                return None;
            }
        }
        Some(items.into_iter().map(|(_, v)| v).collect())
    }

    /// A capability's argv.
    fn argv(&mut self, tree: &Tree, path: &str) -> Option<Vec<Vec<u8>>> {
        let items = self.list(tree, path, "a list of strings")?;
        if items.is_empty() {
            self.error(format!(
                "{path}: is empty; a command needs at least its program"
            ));
            return None;
        }
        let mut argv = Vec::with_capacity(items.len());
        let mut ok = true;
        for (i, item) in items.iter().enumerate() {
            let at = format!("{path}[{}]", i + 1);
            match item {
                Tree::String(s) if s.contains(&0) => {
                    self.error(format!("{at}: holds a NUL byte"));
                    ok = false;
                }
                Tree::String(s) if s.is_empty() && i == 0 => {
                    self.error(format!("{at}: the program is empty"));
                    ok = false;
                }
                Tree::String(s) => argv.push(s.clone()),
                other => {
                    self.error(format!("{at}: {}", expected("string", other)));
                    ok = false;
                }
            }
        }
        ok.then_some(argv)
    }

    /// Checks `argv`'s placeholders: false if the capability is ignored.
    fn placeholders(&mut self, cap: Capability, argv: &[Vec<u8>], path: &str) -> bool {
        for word in argv {
            if let Some(name) = whole_placeholder(word)
                && !cap.gets(name)
            {
                self.warn(format!(
                    "{path}: {{{name}}} isn't a placeholder {c} gets; {c} ignored",
                    c = cap.name()
                ));
                return false;
            }
        }
        for (i, word) in argv.iter().enumerate() {
            if whole_placeholder(word).is_some() || is_escaped(word) {
                continue;
            }
            if let Some(name) = inner_placeholder(word, cap) {
                self.warn(format!(
                    "{path}[{}]: {{{name}}} inside a longer word isn't replaced; passed as written",
                    i + 1
                ));
            }
        }
        true
    }

    fn name(&mut self, tree: &Tree, path: &str) -> Option<String> {
        match tree {
            Tree::String(s) if !s.is_empty() && !s.iter().any(|&b| b < 0x20 || b == 0x7f) => {
                Some(String::from_utf8_lossy(s).into_owned())
            }
            Tree::String(_) => {
                self.error(format!(
                    "{path}: must be a non-empty name without control characters"
                ));
                None
            }
            other => {
                self.error(format!("{path}: {}", expected("string", other)));
                None
            }
        }
    }

    /// Seconds in (0, 60]: over 60 warns and is cut.
    fn seconds(&mut self, tree: &Tree, path: &str) -> Option<Duration> {
        let Tree::Number(n) = tree else {
            self.error(format!("{path}: {}", expected("number", tree)));
            return None;
        };
        if !n.is_finite() || *n <= 0.0 {
            self.error(format!(
                "{path}: must be a number of seconds over 0, got {}",
                number(*n)
            ));
            return None;
        }
        if *n > MAX_TIMEOUT.as_secs_f64() {
            self.warn(format!(
                "{path}: {} s is over {} s; {} s used",
                number(*n),
                MAX_TIMEOUT.as_secs(),
                MAX_TIMEOUT.as_secs()
            ));
            return Some(MAX_TIMEOUT);
        }
        Some(Duration::from_secs_f64(*n))
    }

    fn timeouts(&mut self, tree: &Tree, path: &str) -> Vec<(Capability, Duration)> {
        let Tree::Table(pairs) = tree else {
            self.error(format!("{path}: {}", expected("table", tree)));
            return vec![];
        };
        let mut out = Vec::new();
        for (k, v) in pairs {
            if matches!(v, Tree::Bool(false)) {
                continue;
            }
            let at = format!("{path}{}", segment(k));
            match key_bytes(k).and_then(Capability::from_name) {
                Some(Capability::Events) => self.warn(format!(
                    "{at}: events runs until it ends, with no timeout; ignored"
                )),
                Some(cap) => {
                    if let Some(t) = self.seconds(v, &at) {
                        out.push((cap, t));
                    }
                }
                None => self.warn(format!("{at}: no such capability; ignored")),
            }
        }
        out
    }

    fn protocols(&mut self, tree: &Tree, path: &str) -> Option<Vec<i64>> {
        let items = self.list(tree, path, "a list of protocol numbers")?;
        if items.is_empty() {
            self.error(format!(
                "{path}: is empty; give at least one protocol number"
            ));
            return None;
        }
        let mut out = Vec::new();
        for (i, item) in items.iter().enumerate() {
            match item {
                Tree::Number(n) if *n >= 1.0 && n.fract() == 0.0 && *n <= 1e15 => {
                    out.push(*n as i64)
                }
                Tree::Number(n) => {
                    self.error(format!(
                        "{path}[{}]: expected a positive whole number, got {}",
                        i + 1,
                        number(*n)
                    ));
                    return None;
                }
                other => {
                    self.error(format!("{path}[{}]: {}", i + 1, expected("number", other)));
                    return None;
                }
            }
        }
        Some(out)
    }

    /// A list of variable names, without duplicates.
    fn vars(&mut self, tree: &Tree, path: &str) -> Vec<String> {
        let Some(items) = self.list(tree, path, "a list of variable names") else {
            return vec![];
        };
        let mut out: Vec<String> = Vec::new();
        for (i, item) in items.iter().enumerate() {
            let at = format!("{path}[{}]", i + 1);
            match item {
                Tree::String(s) if is_var_name(s) => {
                    let s = String::from_utf8_lossy(s).into_owned();
                    if !out.contains(&s) {
                        out.push(s);
                    }
                }
                Tree::String(s) => self.error(format!("{at}: {} isn't a variable name", quoted(s))),
                other => self.error(format!("{at}: {}", expected("string", other))),
            }
        }
        out
    }
}

/// Whether `tree` is a pair or one side alone: it has `terminal` or
/// `layout`, other than `false`.
fn is_pair(tree: &Tree) -> bool {
    PairKey::SIDES
        .iter()
        .any(|k| !matches!(tree.get(k.name()), None | Some(Tree::Bool(false))))
}

/// The highest protocol number both the table and the daemon speak.
fn agree(protocols: &[i64]) -> Option<i64> {
    protocols
        .iter()
        .copied()
        .filter(|p| PANE_PROVIDER_PROTOCOLS.contains(p))
        .max()
}

fn is_key(k: &Tree, name: &str) -> bool {
    matches!(k, Tree::String(s) if s == name.as_bytes())
}

fn key_bytes(k: &Tree) -> Option<&[u8]> {
    match k {
        Tree::String(s) => Some(s),
        _ => None,
    }
}

/// `[A-Za-z_][A-Za-z0-9_]*`.
fn is_var_name(s: &[u8]) -> bool {
    match s.split_first() {
        Some((first, rest)) => {
            (first.is_ascii_alphabetic() || *first == b'_')
                && rest.iter().all(|b| b.is_ascii_alphanumeric() || *b == b'_')
        }
        None => false,
    }
}

/// A key as a step in a path: `.key`, `["odd key"]` or `[n]`.
fn segment(k: &Tree) -> String {
    match k {
        Tree::String(s) if is_var_name(s) => format!(".{}", String::from_utf8_lossy(s)),
        Tree::String(s) => format!("[{}]", quoted(s)),
        Tree::Number(n) => format!("[{}]", number(*n)),
        Tree::Bool(b) => format!("[{b}]"),
        other => format!("[{}]", other.type_name()),
    }
}

/// A key as a message shows it.
fn key_shown(k: &Tree) -> String {
    match k {
        Tree::String(s) if is_var_name(s) => String::from_utf8_lossy(s).into_owned(),
        Tree::String(s) => quoted(s),
        other => segment(other),
    }
}

/// A string in double quotes, as Lua would write it.
fn quoted(s: &[u8]) -> String {
    let mut out = String::from("\"");
    for c in String::from_utf8_lossy(s).chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            c if c.is_control() => {
                let _ = write!(out, "\\{}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// A number as Lua's `tostring` would show it.
pub(super) fn number(n: f64) -> String {
    if n.is_nan() {
        "nan".into()
    } else if n.is_infinite() {
        if n > 0.0 { "inf" } else { "-inf" }.into()
    } else if n.fract() == 0.0 && n.abs() < 1e15 {
        format!("{}", n as i64)
    } else {
        format!("{n}")
    }
}

/// A value's kind, with an article: `nil`, `a string`, `a cycle`.
fn a(tree: &Tree) -> String {
    match tree {
        Tree::Nil => "nil".into(),
        Tree::Bool(b) => format!("{b}"),
        Tree::String(s) => quoted(s),
        Tree::Number(n) => format!("the number {}", number(*n)),
        Tree::Table(_) => "a table".into(),
        Tree::Marker(super::tree::Marker::Userdata) => "userdata".into(),
        other => format!("a {}", other.type_name()),
    }
}

/// A value's type, with an article: `nil`, `a string`, `userdata`.
fn kind(tree: &Tree) -> String {
    match tree {
        Tree::Nil => "nil".into(),
        Tree::Marker(super::tree::Marker::Userdata) => "userdata".into(),
        other => format!("a {}", other.type_name()),
    }
}

/// `expected <want>, got <type>`, with a hint for a number where a string
/// belongs.
fn expected(want: &str, got: &Tree) -> String {
    let hint = if want == "string" && matches!(got, Tree::Number(_)) {
        " (wrap numbers in tostring())"
    } else {
        ""
    };
    format!("expected {want}, got {}{hint}", got.type_name())
}

/// The first of `cap`'s placeholders written inside `word`.
fn inner_placeholder(word: &[u8], cap: Capability) -> Option<&'static str> {
    let all = EVERY_CAPABILITY_GETS
        .iter()
        .chain(cap.placeholders())
        .copied();
    for name in all {
        let pattern = format!("{{{name}}}");
        let p = pattern.as_bytes();
        let mut from = 0;
        while let Some(at) = find(&word[from..], p).map(|i| i + from) {
            let doubled = at > 0 && word[at - 1] == b'{' && word.get(at + p.len()) == Some(&b'}');
            if !doubled {
                return Some(name);
            }
            from = at + 1;
        }
    }
    None
}

fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::tree::Marker;

    fn s(text: &str) -> Tree {
        Tree::String(text.as_bytes().to_vec())
    }

    fn list(items: Vec<Tree>) -> Tree {
        Tree::Table(
            items
                .into_iter()
                .enumerate()
                .map(|(i, v)| (Tree::Number(i as f64 + 1.0), v))
                .collect(),
        )
    }

    /// An answer whose `provider` returned `tree`.
    fn returned(tree: Tree) -> Answer {
        Answer {
            build: "b".into(),
            ran: Ran::Returned {
                file: Tree::Table(vec![(s("provider"), Tree::Marker(Marker::Function))]),
                provider: Called::Returned(tree),
            },
        }
    }

    #[test]
    fn a_marker_where_a_value_is_read_is_an_error() {
        let tree = Tree::Table(vec![(
            s("open"),
            list(vec![s("x"), Tree::Marker(Marker::Cycle)]),
        )]);
        let problems = check(&returned(tree), "init.lua").unwrap_err();
        assert_eq!(
            problems.errors(),
            ["provider().open[2]: expected string, got cycle"]
        );
    }

    #[test]
    fn a_marker_where_nothing_is_read_is_only_unknown() {
        let tree = Tree::Table(vec![
            (s("open"), list(vec![s("x")])),
            (s("helper"), Tree::Marker(Marker::Function)),
        ]);
        let chosen = check(&returned(tree), "init.lua").unwrap();
        assert_eq!(
            chosen.warnings.warnings(),
            ["provider().helper: unknown field; ignored"]
        );
    }

    #[test]
    fn the_neovim_message_takes_the_first_error_else_the_first_warning() {
        let mut p = Problems::default();
        assert_eq!(p.message(), None);
        p.push(Severity::Warning, "w".into());
        assert_eq!(p.message().unwrap(), format!("{NAME}: config: w"));
        p.push(Severity::Error, "e".into());
        assert_eq!(
            p.message().unwrap(),
            format!("{NAME}: config: e (and 1 more: {NAME} provider check)")
        );
    }

    #[test]
    fn capabilities_fall_in_three_kinds() {
        let names = |f: fn(Capability) -> bool| -> Vec<&str> {
            Capability::ALL
                .into_iter()
                .filter(|c| f(*c))
                .map(Capability::name)
                .collect()
        };
        assert_eq!(names(Capability::is_terminal), ["open", "close"]);
        assert_eq!(
            names(Capability::is_layout),
            [
                "find",
                "place_beside",
                "resize",
                "focus",
                "focus_direction",
                "report_layout",
                "events"
            ]
        );
        assert_eq!(
            names(Capability::is_one_step),
            ["open_beside", "open_float", "open_tab_page"]
        );
    }
}
