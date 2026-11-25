//! Findings: what the loader reports (design 27 §9).
//!
//! Every finding is a [`miette::Diagnostic`] with a stable code (`gogo_skin::<rule>`, see
//! [`Code`]), the file and span it points at, and a help line. There are three kinds
//! ([`Level`]):
//!
//! - **skin errors** fail the skin, and the default skin is used instead (S12, S16);
//! - **route errors** fail one route, which falls back (design 26 §8);
//! - **warnings** (S18 and friends) never fail anything.
//!
//! A [`Report`] collects them. `gogo-gui --check-skin` (PR E3) prints a report without
//! opening a window.

use std::collections::{BTreeSet, HashMap};
use std::fmt;
use std::sync::Arc;

use miette::{Diagnostic, LabeledSpan, NamedSource, Severity};

use crate::source::{Sources, Span};

macro_rules! codes {
    ($( $(#[$doc:meta])* $variant:ident = $name:literal, )*) => {
        /// The stable rule code of a finding: `gogo_skin::<rule>`. A code is never
        /// reused for another rule.
        #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub enum Code {
            $( $(#[$doc])* $variant, )*
        }

        impl Code {
            /// Every code, in declaration order.
            pub const ALL: &'static [Code] = &[ $( Code::$variant, )* ];

            /// The code as miette shows it: `gogo_skin::path_invalid`.
            pub const fn as_str(self) -> &'static str {
                match self {
                    $( Code::$variant => concat!("gogo_skin::", $name), )*
                }
            }
        }
    };
}

codes! {
    // Package: paths, files and limits (S10, S11, S12).
    /// A path with `..`, an absolute path, a backslash or another unsafe form (S10).
    PathInvalid = "path_invalid",
    /// A symbolic link (S10).
    Symlink = "symlink",
    /// A file that is not KDL, PNG, SVG, OTF or TTF, or is outside the layout (S10).
    FileType = "file_type",
    /// Two files with the same path, or paths that differ only by case.
    DuplicatePath = "duplicate_path",
    /// A file could not be read.
    Io = "io",
    /// A file over its size limit (S12).
    LimitFile = "limit_file",
    /// The skin folder over its size limit (S12).
    LimitFolder = "limit_folder",
    /// More files than the limit (S12).
    LimitFiles = "limit_files",
    /// More nodes than the limit (S12).
    LimitNodes = "limit_nodes",
    /// Nested deeper than the limit (S12).
    LimitDepth = "limit_depth",
    // Assets (S10, S11).
    /// A PNG that is not one.
    AssetPng = "asset_png",
    /// An SVG that is not static, or refers outside itself (S10, S11).
    AssetSvg = "asset_svg",
    /// An OTF or TTF that is not one.
    AssetFont = "asset_font",
    /// A reference to an asset the package does not have, or of the wrong type.
    AssetMissing = "asset_missing",
    /// An asset reference that is not plain text.
    AssetDynamic = "asset_dynamic",
    /// A URL where a skin-relative path belongs (S11).
    UrlScheme = "url_scheme",
    // KDL, templates, expressions.
    /// KDL v2 syntax, or a KDL feature skins don't use.
    KdlSyntax = "kdl_syntax",
    /// A string with `{…}` that doesn't parse (design 27 §7).
    Template = "template",
    /// An expression that doesn't parse, or is over its limits (S12).
    Expression = "expression",
    // Manifest.
    /// The manifest's shape: the `skin` node, missing, repeated or unknown fields.
    ManifestStructure = "manifest_structure",
    /// The skin name.
    SkinName = "skin_name",
    /// A manifest value of the wrong form: title, version, modes, extends.
    ManifestValue = "manifest_value",
    /// A contract version that is malformed or not one this app loads (S16).
    ContractVersion = "contract_version",
    /// A route name the contract doesn't have.
    UnknownRoute = "unknown_route",
    /// Route files and the manifest's `routes` list disagree.
    RouteFile = "route_file",
    /// A `ui { var }` declaration.
    UiVar = "ui_var",
    // Tokens.
    /// The shape of `tokens.kdl`.
    TokenStructure = "token_structure",
    /// A token name that is malformed or reserved.
    TokenName = "token_name",
    /// A token value of the wrong form.
    TokenValue = "token_value",
    /// A token reference that doesn't resolve, or is a cycle, or has the wrong type.
    TokenRef = "token_ref",
    /// Modes that don't define the same tokens.
    TokenMode = "token_mode",
    /// A token the contract requires and the skin doesn't define.
    TokenMissing = "token_missing",
    // Fonts.
    /// A family that is not bundled.
    FontFamily = "font_family",
    /// `stretch` on a static family.
    FontStretch = "font_stretch",
    // Styles.
    /// The shape of `styles.kdl`.
    StyleStructure = "style_structure",
    /// A style name that is malformed or defined twice.
    StyleName = "style_name",
    /// A style that is used and not defined.
    UnknownStyle = "unknown_style",
    /// A state other than the five.
    UnknownState = "unknown_state",
    /// A property the format doesn't have, on this element or in a style.
    UnknownProperty = "unknown_property",
    /// A property value of the wrong type, range or arity.
    PropertyValue = "property_value",
    // Elements.
    /// An element the format doesn't have.
    UnknownElement = "unknown_element",
    /// `scroll`, which is planned (#545).
    PlannedElement = "planned_element",
    /// An element where its parent doesn't allow it.
    ElementPlacement = "element_placement",
    /// Children an element can't have, or children it needs.
    ElementChildren = "element_children",
    /// The wrong number or kind of arguments.
    ElementArguments = "element_arguments",
    /// `use "data-table"`.
    DataTableUse = "data_table_use",
    /// An input variable (`value`, `on`) where there is no input.
    InputScope = "input_scope",
    /// A control or image without an accessible name (S14).
    AccessibleName = "accessible_name",
    /// `each` over something that isn't an expression (S20).
    EachSource = "each_source",
    /// An action name no skin can bind (S16).
    UnknownAction = "unknown_action",
    // Components.
    /// `use` of a component the skin doesn't define.
    UnknownComponent = "unknown_component",
    /// A component declaration or a parameter passed to one.
    ComponentParam = "component_param",
    /// Components that use each other in a loop (S20).
    ComponentCycle = "component_cycle",
    // Warnings.
    /// A status shown by colour alone (S18).
    StatusByColour = "status_by_colour",
    /// A property set twice in one place.
    DuplicateProperty = "duplicate_property",
    /// A `case` that can never match, because an earlier one has the same value.
    DuplicateCase = "duplicate_case",
}

impl fmt::Display for Code {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// How serious a finding is, and what it fails.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Level {
    /// The skin fails to load (S12, S16): the default skin is used.
    Skin,
    /// One route fails: it falls back (design 26 §8).
    Route(String),
    /// Reported, and nothing fails.
    Warning,
}

/// One finding: a rule, where it was broken, and how to fix it.
#[derive(Debug, Clone)]
pub struct Finding {
    pub code: Code,
    pub level: Level,
    pub message: String,
    pub help: Option<String>,
    /// Where it points; `None` for a finding about a path or the package as a whole.
    pub span: Option<Span>,
    /// `routes/game.kdl:12:5`, or the path alone for a file that is not KDL.
    pub location: String,
    /// The text the span covers, cut at 200 characters.
    pub text: Option<String>,
    source: Option<NamedSource<Arc<str>>>,
    label: Option<String>,
}

impl Finding {
    fn plain(code: Code, level: Level, message: String, path: &str) -> Self {
        Finding {
            code,
            level,
            message,
            help: None,
            span: None,
            location: path.to_owned(),
            text: None,
            source: None,
            label: None,
        }
    }

    pub fn is_error(&self) -> bool {
        !matches!(self.level, Level::Warning)
    }
}

impl fmt::Display for Finding {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for Finding {}

impl Diagnostic for Finding {
    fn code<'a>(&'a self) -> Option<Box<dyn fmt::Display + 'a>> {
        Some(Box::new(self.code))
    }

    fn severity(&self) -> Option<Severity> {
        Some(if self.is_error() {
            Severity::Error
        } else {
            Severity::Warning
        })
    }

    fn help<'a>(&'a self) -> Option<Box<dyn fmt::Display + 'a>> {
        self.help
            .as_deref()
            .map(|h| Box::new(h) as Box<dyn fmt::Display + 'a>)
    }

    fn source_code(&self) -> Option<&dyn miette::SourceCode> {
        self.source.as_ref().map(|s| s as &dyn miette::SourceCode)
    }

    fn labels(&self) -> Option<Box<dyn Iterator<Item = LabeledSpan> + '_>> {
        let span = self.span?;
        self.source.as_ref()?;
        Some(Box::new(std::iter::once(LabeledSpan::new_with_span(
            self.label.clone(),
            (span.offset, span.len.max(1)),
        ))))
    }
}

/// How many findings a report keeps; the rest are counted in [`Report::suppressed`].
pub const MAX_FINDINGS: usize = 200;

/// Every finding of one load.
#[derive(Debug, Clone, Default)]
pub struct Report {
    findings: Vec<Finding>,
    suppressed: usize,
}

impl Report {
    pub fn findings(&self) -> &[Finding] {
        &self.findings
    }

    /// Findings dropped after [`MAX_FINDINGS`].
    pub fn suppressed(&self) -> usize {
        self.suppressed
    }

    pub fn is_empty(&self) -> bool {
        self.findings.is_empty()
    }

    /// Nothing at all to report: no errors and no warnings.
    pub fn is_clean(&self) -> bool {
        self.findings.is_empty() && self.suppressed == 0
    }

    pub fn errors(&self) -> impl Iterator<Item = &Finding> {
        self.findings.iter().filter(|f| f.is_error())
    }

    pub fn warnings(&self) -> impl Iterator<Item = &Finding> {
        self.findings.iter().filter(|f| !f.is_error())
    }

    /// The errors that fail the whole skin.
    pub fn skin_errors(&self) -> impl Iterator<Item = &Finding> {
        self.findings
            .iter()
            .filter(|f| matches!(f.level, Level::Skin))
    }

    /// The errors that fail one route.
    pub fn route_errors(&self) -> impl Iterator<Item = &Finding> {
        self.findings
            .iter()
            .filter(|f| matches!(f.level, Level::Route(_)))
    }

    pub fn has_skin_errors(&self) -> bool {
        self.skin_errors().next().is_some()
    }

    /// The routes that fell back, sorted.
    pub fn failed_routes(&self) -> BTreeSet<&str> {
        self.findings
            .iter()
            .filter_map(|f| match &f.level {
                Level::Route(r) => Some(r.as_str()),
                _ => None,
            })
            .collect()
    }

    fn push(&mut self, f: Finding) -> Option<usize> {
        if self.findings.len() >= MAX_FINDINGS {
            self.suppressed += 1;
            None
        } else {
            self.findings.push(f);
            Some(self.findings.len() - 1)
        }
    }
}

impl fmt::Display for Report {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let errors = self.errors().count();
        let warnings = self.warnings().count();
        write!(f, "skin: {errors} error(s), {warnings} warning(s)")?;
        if self.suppressed > 0 {
            write!(f, ", {} more not shown", self.suppressed)?;
        }
        for x in &self.findings {
            let kind = if x.is_error() { "error" } else { "warning" };
            write!(f, "\n  {}: {kind}[{}]: {}", x.location, x.code, x.message)?;
        }
        Ok(())
    }
}

impl std::error::Error for Report {}

impl Diagnostic for Report {
    fn code<'a>(&'a self) -> Option<Box<dyn fmt::Display + 'a>> {
        Some(Box::new("gogo_skin::report"))
    }

    fn related<'a>(&'a self) -> Option<Box<dyn Iterator<Item = &'a dyn Diagnostic> + 'a>> {
        Some(Box::new(self.findings.iter().map(|x| x as &dyn Diagnostic)))
    }
}

/// Where the loader is when it finds something: decides the [`Level`] of an error.
#[derive(Debug, Clone, Default)]
pub(crate) enum Scope {
    /// skin.kdl, tokens.kdl, styles.kdl, components and assets.
    #[default]
    Skin,
    /// `routes/<name>.kdl`.
    Route(String),
}

/// Collects findings while the loader works.
#[derive(Debug, Default)]
pub(crate) struct Collector {
    pub sources: Sources,
    pub report: Report,
    pub scope: Scope,
    cache: HashMap<usize, NamedSource<Arc<str>>>,
}

impl Collector {
    fn level(&self) -> Level {
        match &self.scope {
            Scope::Skin => Level::Skin,
            Scope::Route(r) => Level::Route(r.clone()),
        }
    }

    fn add(&mut self, code: Code, level: Level, span: Span, msg: String) -> FindingRef<'_> {
        let mut f = Finding::plain(code, level, msg, "");
        if let Some(file) = self.sources.get(span.file) {
            f.location = self.sources.location(span);
            f.span = Some(span);
            f.text = self
                .sources
                .slice(span)
                .map(|t| t.chars().take(200).collect());
            let src = self
                .cache
                .entry(span.file)
                .or_insert_with(|| NamedSource::new(file.path.clone(), file.text.clone()))
                .clone();
            f.source = Some(src);
        }
        let idx = self.report.push(f);
        FindingRef { col: self, idx }
    }

    /// An error at a span, at the level of the current scope.
    pub fn error(&mut self, code: Code, span: Span, msg: impl Into<String>) -> FindingRef<'_> {
        let level = self.level();
        self.add(code, level, span, msg.into())
    }

    /// An error that fails the whole skin, whatever the scope.
    pub fn skin_error(&mut self, code: Code, span: Span, msg: impl Into<String>) -> FindingRef<'_> {
        self.add(code, Level::Skin, span, msg.into())
    }

    pub fn warn(&mut self, code: Code, span: Span, msg: impl Into<String>) -> FindingRef<'_> {
        self.add(code, Level::Warning, span, msg.into())
    }

    fn add_path(&mut self, code: Code, level: Level, path: &str, msg: String) -> FindingRef<'_> {
        let idx = self.report.push(Finding::plain(code, level, msg, path));
        FindingRef { col: self, idx }
    }

    /// A skin error about a path, with no KDL source.
    pub fn path_error(&mut self, code: Code, path: &str, msg: impl Into<String>) -> FindingRef<'_> {
        self.add_path(code, Level::Skin, path, msg.into())
    }

    /// How many errors there are so far, at any level: a stage tells whether it added one.
    pub fn error_count(&self) -> usize {
        self.report.errors().count() + self.report.suppressed
    }
}

/// A just-added finding, to set its help or label.
pub(crate) struct FindingRef<'a> {
    col: &'a mut Collector,
    idx: Option<usize>,
}

impl FindingRef<'_> {
    pub fn help(self, help: impl Into<String>) -> Self {
        if let Some(i) = self.idx {
            self.col.report.findings[i].help = Some(help.into());
        }
        self
    }

    pub fn label(self, label: impl Into<String>) -> Self {
        if let Some(i) = self.idx {
            self.col.report.findings[i].label = Some(label.into());
        }
        self
    }
}
