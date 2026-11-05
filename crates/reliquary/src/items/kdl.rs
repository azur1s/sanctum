//! Reads items and skills from KDL (v2) files.
//!
//! Every unknown node or option is an error with a file:line:col position,
//! so typos show up on load instead of silently doing nothing.

use std::cell::RefCell;
use std::fmt;
use std::sync::Arc;

use ::kdl::{KdlDocument, KdlEntry, KdlNode, KdlValue};

use super::*;
use super::expr;

#[derive(Debug, Clone)]
pub struct LoadError {
    pub file: String,
    pub line: usize,
    pub col: usize,
    pub message: String,
}

impl fmt::Display for LoadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}:{}: {}", self.file, self.line, self.col, self.message)
    }
}

type R<T> = Result<T, LoadError>;

/// Everything defined in one file.
#[derive(Debug, Default)]
pub struct Parsed {
    pub items: Vec<ItemFile>,
    pub skills: Vec<SkillDef>,
    /// `self.<stat>` uses, with "file:line:col", to check once all items
    /// (and so all stat names) are known.
    pub stat_refs: Vec<(String, String)>,
    /// Custom stats declared with `stat "magic-power"` (snake_case).
    pub declared_stats: Vec<String>,
    /// `damage ... kind="x"` uses, with "file:line:col", checked against kinds.kdl.
    pub kind_refs: Vec<(String, String)>,
}

/// Parses `kinds.kdl`: only `kind` nodes.
pub fn parse_kinds(file: &str, src: &str) -> R<Vec<DamageKind>> {
    let p = Parser { file, src, used_params: RefCell::default(), used_stats: RefCell::default(), used_kinds: RefCell::default() };
    let doc = p.document()?;
    let mut kinds: Vec<DamageKind> = Vec::new();
    for node in doc.nodes() {
        if name(node) != "kind" {
            return Err(p.at(node, format!("kinds.kdl only holds `kind` nodes, found `{}`", name(node))));
        }
        let id = p.id(node, "a kind id, like `kind \"magic\" { ... }`")?;
        p.shape(node, 1, &[], true)?;
        if kinds.iter().any(|k| k.id == id) {
            return Err(p.at(node, format!("kind `{id}` is defined twice")));
        }
        let (mut power, mut defense, mut source) = (None, None, None);
        let mut display = KindDisplay::default();
        for child in children(node) {
            match name(child) {
                "power" => {
                    let entry = p.arg(child, 0, "a stat, like `power \"magic-power\"`")?;
                    p.shape(child, 1, &[], false)?;
                    let stat = p.string(entry)?.replace('-', "_");
                    p.stat_name(entry, &stat)?;
                    power = Some(stat);
                }
                "defense" => {
                    let entry = p.arg(child, 0, "a stat, like `defense \"magic-armor\"`")?;
                    p.shape(child, 1, &[], false)?;
                    let stat = p.string(entry)?.replace('-', "_");
                    p.stat_name(entry, &stat)?;
                    defense = Some(stat);
                }
                "source" => source = Some(p.str_arg(child, "a Minecraft damage type, like \"magic\"")?),
                "display" => {
                    p.shape(child, 0, &[], true)?;
                    for field in children(child) {
                        match name(field) {
                            "prefix" => display.prefix = p.str_arg(field, "text, like an icon")?,
                            "color" => display.color = p.str_arg(field, "color codes, like \"&d\"")?,
                            other => {
                                return Err(p.at(field, format!("unknown display field `{other}` (expected prefix or color)")));
                            }
                        }
                    }
                }
                other => return Err(p.at(child, format!("unknown kind field `{other}` (expected power, defense, source or display)"))),
            }
        }
        kinds.push(DamageKind { id, power, defense, display, source: source.unwrap_or_else(|| "generic".into()) });
    }
    Ok(kinds)
}

/// Parses `item_rarity.kdl`: only `rarity` nodes, kept in file order.
pub fn parse_rarities(file: &str, src: &str) -> R<Vec<RarityDef>> {
    let p = Parser { file, src, used_params: RefCell::default(), used_stats: RefCell::default(), used_kinds: RefCell::default() };
    let doc = p.document()?;
    let mut rarities: Vec<RarityDef> = Vec::new();
    for node in doc.nodes() {
        if name(node) != "rarity" {
            return Err(p.at(node, format!("item_rarity.kdl only holds `rarity` nodes, found `{}`", name(node))));
        }
        let id = p.id(node, "a rarity id, like `rarity \"common\" { ... }`")?;
        p.shape(node, 1, &[], true)?;
        if rarities.iter().any(|r| r.id == id) {
            return Err(p.at(node, format!("rarity `{id}` is defined twice")));
        }
        let (mut rarity_name, mut color) = (None, None);
        for child in children(node) {
            match name(child) {
                "name" => rarity_name = Some(p.str_arg(child, "a name shown in the lore, like \"Common\"")?),
                "color" => color = Some(p.str_arg(child, "color codes, like \"&7\"")?),
                other => return Err(p.at(child, format!("unknown rarity field `{other}` (expected name or color)"))),
            }
        }
        rarities.push(RarityDef {
            name: rarity_name.ok_or_else(|| p.at(node, format!("rarity `{id}` needs a `name`")))?,
            color: color.ok_or_else(|| p.at(node, format!("rarity `{id}` needs a `color`")))?,
            id,
        });
    }
    Ok(rarities)
}

/// Parses `reforges.kdl`: only `reforge` nodes. Also returns each stat with its
/// "file:line:col", to check custom stats are declared.
pub fn parse_reforges(file: &str, src: &str) -> R<(Vec<ReforgeDef>, Vec<(String, String)>)> {
    let p = Parser { file, src, used_params: RefCell::default(), used_stats: RefCell::default(), used_kinds: RefCell::default() };
    let doc = p.document()?;
    let mut reforges: Vec<ReforgeDef> = Vec::new();
    let mut stat_refs = Vec::new();
    for node in doc.nodes() {
        if name(node) != "reforge" {
            return Err(p.at(node, format!("reforges.kdl only holds `reforge` nodes, found `{}`", name(node))));
        }
        let id = p.id(node, "a reforge id, like `reforge \"sharp\" { ... }`")?;
        p.shape(node, 1, &[], true)?;
        if reforges.iter().any(|r| r.id == id) {
            return Err(p.at(node, format!("reforge `{id}` is defined twice")));
        }
        let mut reforge = ReforgeDef { id, name: String::new(), weight: 1.0, slots: Vec::new(), kinds: Vec::new(), stats: Stats::new() };
        let mut has_name = false;
        for child in children(node) {
            match name(child) {
                "name" => {
                    reforge.name = p.str_arg(child, "a name put before the item's, like \"Sharp\"")?;
                    has_name = true;
                }
                "weight" => {
                    reforge.weight = p.num_arg(child, "a number above 0")?;
                    if reforge.weight <= 0.0 {
                        return Err(p.at(child, "`weight` must be above 0"));
                    }
                }
                "slots" => {
                    p.shape(child, usize::MAX, &[], false)?;
                    for entry in args(child) {
                        let value = p.string(entry)?;
                        reforge.slots.push(Slot::parse(&value).ok_or_else(|| {
                            p.at_entry(entry, format!("unknown slot `{value}` (expected mainhand, offhand, head, chest, legs or feet)"))
                        })?);
                    }
                }
                "kinds" => {
                    p.shape(child, usize::MAX, &[], false)?;
                    for entry in args(child) {
                        reforge.kinds.push(p.string(entry)?);
                    }
                }
                "stats" => {
                    let (stats, at) = p.stats(child)?;
                    reforge.stats = stats;
                    stat_refs.extend(at);
                    if !reforge.slots.is_empty()
                        && !reforge.slots.contains(&Slot::MainHand)
                        && let Some(stat) = reforge.stats.keys().find(|s| stat_info(s).is_some_and(|i| i.main_hand_only))
                    {
                        return Err(p.at(child, format!("`{}` only works on main-hand items, but `slots` leaves those out", stat.replace('_', "-"))));
                    }
                }
                other => return Err(p.at(child, format!("unknown reforge field `{other}` (expected name, weight, slots, kinds or stats)"))),
            }
        }
        if !has_name {
            return Err(p.at(node, format!("reforge `{}` needs a `name`", reforge.id)));
        }
        reforges.push(reforge);
    }
    Ok((reforges, stat_refs))
}

/// Parses `stats_format.kdl`: one optional top-level `format`, then `stat`
/// nodes in lore order. Also returns each stat with its "file:line:col", to
/// check against the known stats once every item is loaded.
pub fn parse_stat_formats(file: &str, src: &str) -> R<(StatFormats, Vec<(String, String)>)> {
    let p = Parser { file, src, used_params: RefCell::default(), used_stats: RefCell::default(), used_kinds: RefCell::default() };
    let doc = p.document()?;
    let mut formats = StatFormats::default();
    let mut refs = Vec::new();
    let mut format_node = None;
    for node in doc.nodes() {
        match name(node) {
            "format" => {
                if format_node.replace(node).is_some() {
                    return Err(p.at(node, "`format` is set twice"));
                }
                formats.format = p.stat_format(node)?;
            }
            "stat" => {
                p.shape(node, 1, &[], true)?;
                let entry = p.arg(node, 0, "a stat, like `stat \"magic-power\" { ... }`")?;
                let stat = p.string(entry)?.replace('-', "_");
                p.stat_name(entry, &stat)?;
                if formats.stats.iter().any(|f| f.stat == stat) {
                    return Err(p.at(node, format!("stat `{stat}` is listed twice")));
                }
                let mut f = StatFormat { stat, name: None, format: None, hidden: false };
                for child in children(node) {
                    match name(child) {
                        "name" => f.name = Some(p.str_arg(child, "a name shown in the lore, like \"Magic Power\"")?),
                        "format" => f.format = Some(p.stat_format(child)?),
                        "hidden" => {
                            p.shape(child, 1, &[], false)?;
                            f.hidden = p.boolean(p.arg(child, 0, "#true or #false")?)?;
                        }
                        other => {
                            return Err(p.at(child, format!("unknown stat field `{other}` (expected name, format or hidden)")));
                        }
                    }
                }
                refs.push((f.stat.clone(), p.position(node)));
                formats.stats.push(f);
            }
            other => return Err(p.at(node, format!("expected `format` or `stat`, found `{other}`"))),
        }
    }
    Ok((formats, refs))
}

/// Parses `stats_layout.kdl`: one node per lore entry, in lore order. Also
/// returns each `stat` with its "file:line:col", to check against the known
/// stats once every item is loaded.
pub fn parse_stats_layout(file: &str, src: &str) -> R<(Vec<LoreEntry>, Vec<(String, String)>)> {
    let p = Parser { file, src, used_params: RefCell::default(), used_stats: RefCell::default(), used_kinds: RefCell::default() };
    let doc = p.document()?;
    let mut layout = Vec::new();
    let mut refs = Vec::new();
    for node in doc.nodes() {
        let entry = match name(node) {
            "empty" => LoreEntry::Empty,
            "rarity" => LoreEntry::Rarity,
            "lore" => LoreEntry::Lore,
            "stats" => LoreEntry::Stats,
            "skills" => LoreEntry::Skills,
            "sockets" => LoreEntry::Sockets,
            "divider" => {
                p.shape(node, 1, &["exists", "color", "fade"], false)?;
                let divider = Divider {
                    title: args(node).next().map(|e| p.string(e)).transpose()?.unwrap_or_default(),
                    color: prop(node, "color").map(|e| p.rgb(e)).transpose()?.unwrap_or(Divider::DEFAULT_COLOR),
                    fade: prop(node, "fade").map(|e| p.rgb(e)).transpose()?.unwrap_or(Divider::DEFAULT_FADE),
                };
                let exists = prop(node, "exists").map(|e| p.lore_section(e)).transpose()?.map(Box::new);
                LoreEntry::Divider { divider, exists }
            }
            "stat" => {
                p.shape(node, 1, &[], false)?;
                let entry = p.arg(node, 0, "a stat, like `stat \"magic-power\"`")?;
                let stat = p.string(entry)?.replace('-', "_");
                p.stat_name(entry, &stat)?;
                refs.push((stat.clone(), p.position(node)));
                LoreEntry::Stat(stat)
            }
            other => {
                return Err(p.at(node, format!("unknown lore entry `{other}` (expected rarity, lore, stat, stats, sockets, skills, divider or empty)")));
            }
        };
        if !matches!(entry, LoreEntry::Stat(_) | LoreEntry::Divider { .. }) {
            p.shape(node, 0, &[], false)?;
        }
        if !matches!(entry, LoreEntry::Empty | LoreEntry::Divider { .. }) && layout.contains(&entry) {
            return Err(p.at(node, format!("lore entry `{}` is listed twice", name(node))));
        }
        layout.push(entry);
    }
    Ok((layout, refs))
}

/// Parses one file. A file may contain any number of `item` and `skill` nodes.
pub fn parse_file(file: &str, src: &str) -> R<Parsed> {
    let p = Parser { file, src, used_params: RefCell::default(), used_stats: RefCell::default(), used_kinds: RefCell::default() };
    let doc = p.document()?;
    let mut parsed = Parsed::default();
    for node in doc.nodes() {
        match name(node) {
            "item" => parsed.items.push(p.item(node)?),
            "stat" => {
                p.shape(node, 1, &[], false)?;
                let entry = p.arg(node, 0, "a stat name, like `stat \"magic-power\"`")?;
                let stat = p.string(entry)?.replace('-', "_");
                if stat_info(&stat).is_some() {
                    return Err(p.at_entry(entry, format!("`{stat}` is built in; no need to declare it")));
                }
                p.stat_name(entry, &stat)?;
                parsed.declared_stats.push(stat);
            }
            "skill" => {
                let id = p.id(node, "an id, like `skill \"moon_slash\" { ... }`")?;
                if node.children().is_none() {
                    return Err(p.at(node, "a skill in the library needs a `{ ... }` block"));
                }
                parsed.skills.push(p.skill(node, id)?);
            }
            other => return Err(p.at(node, format!("expected `item` or `skill`, found `{other}`"))),
        }
    }
    for (stat, offset) in p.used_stats.borrow().iter() {
        parsed.stat_refs.push((stat.clone(), p.position_at(*offset)));
    }
    for (kind, offset) in p.used_kinds.borrow().iter() {
        parsed.kind_refs.push((kind.clone(), p.position_at(*offset)));
    }
    Ok(parsed)
}

struct Parser<'a> {
    file: &'a str,
    src: &'a str,
    /// `$params` used in the skill being parsed, with their positions.
    used_params: RefCell<Vec<(String, usize)>>,
    /// `self.stats` used anywhere in the file, with their positions.
    used_stats: RefCell<Vec<(String, usize)>>,
    /// `kind="x"` used anywhere in the file, with their positions.
    used_kinds: RefCell<Vec<(String, usize)>>,
}

fn name(node: &KdlNode) -> &str {
    node.name().value()
}

fn args(node: &KdlNode) -> impl Iterator<Item = &KdlEntry> {
    node.entries().iter().filter(|e| e.name().is_none())
}

fn prop<'n>(node: &'n KdlNode, key: &str) -> Option<&'n KdlEntry> {
    node.entries()
        .iter()
        .find(|e| e.name().is_some_and(|n| n.value() == key))
}

fn children(node: &KdlNode) -> &[KdlNode] {
    node.children().map_or(&[], |d| d.nodes())
}

impl Parser<'_> {
    fn err(&self, offset: usize, message: impl Into<String>) -> LoadError {
        let before = self.src.get(..offset).unwrap_or(self.src);
        let line = before.matches('\n').count() + 1;
        let col = before.rsplit('\n').next().map_or(0, |l| l.chars().count()) + 1;
        LoadError {
            file: self.file.to_string(),
            line,
            col,
            message: message.into(),
        }
    }

    fn at(&self, node: &KdlNode, message: impl Into<String>) -> LoadError {
        self.err(node.span().offset(), message)
    }

    fn at_entry(&self, entry: &KdlEntry, message: impl Into<String>) -> LoadError {
        self.err(entry.span().offset(), message)
    }

    fn position(&self, node: &KdlNode) -> String {
        self.position_at(node.span().offset())
    }

    fn position_at(&self, offset: usize) -> String {
        let e = self.err(offset, "");
        format!("{}:{}:{}", e.file, e.line, e.col)
    }

    /// Parses the whole source as a KDL document.
    fn document(&self) -> R<KdlDocument> {
        KdlDocument::parse(self.src).map_err(|e| {
            let Some(d) = e.diagnostics.first() else {
                return self.err(0, "invalid KDL");
            };
            let mut msg = d.message.clone().unwrap_or_else(|| "invalid KDL".into());
            if let Some(help) = &d.help {
                msg = format!("{msg} ({help})");
            }
            self.err(d.span.offset(), msg)
        })
    }

    /// Rejects extra arguments, unknown options and (optionally) children.
    fn shape(&self, node: &KdlNode, max_args: usize, props: &[&str], kids: bool) -> R<()> {
        if let Some(extra) = args(node).nth(max_args) {
            return Err(self.at_entry(extra, format!("too many values for `{}`", name(node))));
        }
        for entry in node.entries() {
            if let Some(key) = entry.name()
                && !props.contains(&key.value())
            {
                let hint = if props.is_empty() {
                    String::from("it takes no options")
                } else {
                    format!("expected one of: {}", props.join(", "))
                };
                return Err(self.at_entry(entry, format!("unknown option `{}` ({hint})", key.value())));
            }
        }
        if !kids && node.children().is_some() {
            return Err(self.at(node, format!("`{}` does not take a block", name(node))));
        }
        Ok(())
    }

    fn arg<'n>(&self, node: &'n KdlNode, i: usize, what: &str) -> R<&'n KdlEntry> {
        args(node)
            .nth(i)
            .ok_or_else(|| self.at(node, format!("`{}` needs {what}", name(node))))
    }

    fn string(&self, entry: &KdlEntry) -> R<String> {
        entry
            .value()
            .as_string()
            .map(str::to_string)
            .ok_or_else(|| self.at_entry(entry, "expected text"))
    }

    /// An id: a-z, 0-9 and _.
    fn id(&self, node: &KdlNode, what: &str) -> R<String> {
        let entry = self.arg(node, 0, what)?;
        let id = self.string(entry)?;
        if id.is_empty() || !id.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_') {
            return Err(self.at_entry(entry, format!("id `{id}` may only use a-z, 0-9 and _")));
        }
        Ok(id)
    }

    fn number(&self, entry: &KdlEntry) -> R<f64> {
        match entry.value() {
            KdlValue::Integer(i) => Ok(*i as f64),
            KdlValue::Float(f) => Ok(*f),
            _ => Err(self.at_entry(entry, "expected a number")),
        }
    }

    fn whole(&self, entry: &KdlEntry) -> R<u32> {
        match entry.value() {
            KdlValue::Integer(i) if (0..=u32::MAX as i128).contains(i) => Ok(*i as u32),
            _ => Err(self.at_entry(entry, "expected a whole number")),
        }
    }

    fn boolean(&self, entry: &KdlEntry) -> R<bool> {
        entry
            .value()
            .as_bool()
            .ok_or_else(|| self.at_entry(entry, "expected #true or #false"))
    }

    /// Records the `$params` and `self.stats` an expression uses, so they can
    /// be checked once the whole skill / all items are known.
    fn record(&self, e: &Expr, offset: usize) {
        e.visit(&mut |node| match node {
            Expr::Param(p) => self.used_params.borrow_mut().push((p.clone(), offset)),
            Expr::Stat(s) => self.used_stats.borrow_mut().push((s.clone(), offset)),
            _ => {}
        });
    }

    /// Where the text of a string value starts in the file, so errors can point
    /// inside it. Works from the end of the entry, which skips any `key=`.
    fn text_start(&self, entry: &KdlEntry, text: &str) -> usize {
        let end = entry.span().offset() + entry.span().len();
        let quoted = self.src.get(..end).is_some_and(|s| s.ends_with('"'));
        end.saturating_sub(text.len() + usize::from(quoted))
    }

    /// Parses a formula written in a string value.
    fn formula(&self, entry: &KdlEntry, src: &str) -> R<Expr> {
        let start = self.text_start(entry, src);
        let e = expr::parse(src).map_err(|e| self.err(start + e.at, format!("in formula: {}", e.message)))?;
        self.record(&e, start);
        Ok(e)
    }

    /// A number or a formula like `"$damage + self.magic_power * 0.25"`.
    fn num(&self, entry: &KdlEntry) -> R<Num> {
        match entry.value() {
            KdlValue::String(s) => self.formula(entry, s).map(Num::Expr),
            _ => self
                .number(entry)
                .map(Num::Lit)
                .map_err(|_| self.at_entry(entry, "expected a number or a formula")),
        }
    }

    /// Ticks: a whole number, text like "2s" / "10t", or a formula (ticks).
    fn ticks(&self, entry: &KdlEntry) -> R<Num> {
        if let KdlValue::String(s) = entry.value() {
            let time = if let Some(secs) = s.strip_suffix('s') {
                secs.parse::<f64>().ok().map(|v| (v * 20.0).round())
            } else {
                s.strip_suffix('t').and_then(|t| t.parse::<u32>().ok()).map(f64::from)
            };
            return match time {
                Some(t) => Ok(Num::Lit(t)),
                None => self.formula(entry, s).map(Num::Expr),
            };
        }
        self.whole(entry).map(|t| Num::Lit(t.into()))
    }

    /// Text that may hold `<expr>` placeholders.
    fn text(&self, entry: &KdlEntry) -> R<String> {
        let s = self.string(entry)?;
        let start = self.text_start(entry, &s);
        let pieces = expr::pieces(&s).map_err(|e| self.err(start + e.at, format!("in placeholder: {}", e.message)))?;
        for piece in &pieces {
            if let expr::Piece::Expr(e) = piece {
                self.record(e, start);
            }
        }
        Ok(s)
    }

    fn str_arg(&self, node: &KdlNode, what: &str) -> R<String> {
        self.shape(node, 1, &[], false)?;
        self.string(self.arg(node, 0, what)?)
    }

    /// A color like "#7a6a9e".
    fn rgb(&self, entry: &KdlEntry) -> R<Rgb> {
        let text = self.string(entry)?;
        let parsed = text.strip_prefix('#').filter(|hex| hex.len() == 6 && hex.is_ascii()).and_then(|hex| {
            let part = |i: usize| u8::from_str_radix(&hex[i..i + 2], 16).ok();
            Some((part(0)?, part(2)?, part(4)?))
        });
        parsed.ok_or_else(|| self.at_entry(entry, format!("`{text}` is not a color like \"#7a6a9e\"")))
    }

    /// The lore section a divider goes with, for `exists`.
    fn lore_section(&self, entry: &KdlEntry) -> R<LoreEntry> {
        let text = self.string(entry)?;
        match text.as_str() {
            "rarity" => Ok(LoreEntry::Rarity),
            "lore" => Ok(LoreEntry::Lore),
            "stats" => Ok(LoreEntry::Stats),
            "sockets" => Ok(LoreEntry::Sockets),
            "skills" => Ok(LoreEntry::Skills),
            _ => Err(self.at_entry(entry, format!("unknown section `{text}` (expected rarity, lore, stats, sockets or skills)"))),
        }
    }

    /// A stat line like "&7{name} &b{sign}{value}"; unknown `{...}` are errors.
    fn stat_format(&self, node: &KdlNode) -> R<String> {
        let format = self.str_arg(node, "a line, like \"&7{name} &b{sign}{value}\"")?;
        let entry = self.arg(node, 0, "")?;
        let start = self.text_start(entry, &format);
        let mut rest = format.as_str();
        while let Some(open) = rest.find('{') {
            let at = start + (format.len() - rest.len()) + open;
            let Some(close) = rest[open..].find('}') else {
                return Err(self.err(at, "`{` is never closed"));
            };
            let key = &rest[open + 1..open + close];
            if !STAT_PLACEHOLDERS.contains(&key) {
                return Err(self.err(at, format!("unknown placeholder `{{{key}}}` (expected {{name}}, {{sign}}, {{value}} or {{fill}})")));
            }
            rest = &rest[open + close + 1..];
        }
        if format.matches("{fill}").count() > 1 {
            return Err(self.at_entry(entry, "`{fill}` can only be used once in a line"));
        }
        Ok(format)
    }

    fn num_arg(&self, node: &KdlNode, what: &str) -> R<f64> {
        self.shape(node, 1, &[], false)?;
        self.number(self.arg(node, 0, what)?)
    }

    fn opt<T>(&self, node: &KdlNode, key: &str, read: impl Fn(&KdlEntry) -> R<T>) -> R<Option<T>> {
        prop(node, key).map(read).transpose()
    }

    /// An option that may be a `$param`, with a default.
    fn opt_num(&self, node: &KdlNode, key: &str, default: f64) -> R<Num> {
        Ok(self.opt(node, key, |e| self.num(e))?.unwrap_or(Num::Lit(default)))
    }

    // ---- Items -------------------------------------------------------------

    fn item(&self, node: &KdlNode) -> R<ItemFile> {
        self.shape(node, 1, &[], true)?;
        let id = self.id(node, "an id, like `item \"moonlit_edge\" { ... }`")?;

        let (mut item_name, mut base, mut model, mut rarity, mut kind) = (None, None, None, None, None);
        let mut slot = None;
        let mut lore = Vec::new();
        let mut stats = Stats::new();
        let mut stats_node = None;
        let mut stat_at = Vec::new();
        let mut uses = Vec::new();
        let (mut sockets, mut gem, mut reforgeable) = (Vec::new(), None, true);

        for child in children(node) {
            match name(child) {
                "sockets" => sockets = self.sockets(child)?,
                "gem" => gem = Some(self.socket_type(self.arg(child, 0, "a socket type, like `gem \"red\"`")?)?),
                "reforgeable" => {
                    self.shape(child, 1, &[], false)?;
                    reforgeable = self.boolean(self.arg(child, 0, "#true or #false")?)?;
                }
                "name" => item_name = Some(self.str_arg(child, "the item name")?),
                "base" => base = Some(self.str_arg(child, "a vanilla item, like \"minecraft:diamond_sword\"")?),
                "model" => model = Some(self.str_arg(child, "an item model id")?),
                "kind" => kind = Some(self.str_arg(child, "a kind, like \"Sword\"")?),
                "rarity" => rarity = Some((self.str_arg(child, "a rarity from item_rarity.kdl")?, self.position(child))),
                "slot" => {
                    let value = self.str_arg(child, "a slot")?;
                    slot = Some(Slot::parse(&value).ok_or_else(|| {
                        self.at(child, format!("unknown slot `{value}` (expected mainhand, offhand, head, chest, legs or feet)"))
                    })?);
                }
                "lore" => {
                    self.shape(child, usize::MAX, &[], false)?;
                    for entry in args(child) {
                        let before = self.used_params.borrow().len();
                        lore.push(self.text(entry)?);
                        if let Some((p, offset)) = self.used_params.borrow().get(before) {
                            return Err(self.err(*offset, format!("`${p}` only works in skills; item lore can use self.<stat>")));
                        }
                    }
                }
                "stats" => {
                    (stats, stat_at) = self.stats(child)?;
                    stats_node = Some(child);
                }
                "skill" => uses.push(self.skill_use(child)?),
                other => return Err(self.at(child, format!("unknown item field `{other}`"))),
            }
        }

        let base = base.ok_or_else(|| self.at(node, format!("item `{id}` needs a `base`")))?;
        let slot = slot.unwrap_or_else(|| Slot::guess(&base));
        if slot != Slot::MainHand {
            if let Some(stat) = stats.keys().find(|s| stat_info(s).is_some_and(|i| i.main_hand_only)) {
                let at = stats_node.map_or(node, |n| n);
                return Err(self.at(at, format!("`{}` only works on main-hand items, but this item goes in {slot:?}", stat.replace('_', "-"))));
            }
        }

        if gem.is_some() && !sockets.is_empty() {
            return Err(self.at(node, format!("item `{id}` is a gem, so it can't have sockets of its own")));
        }

        let item = ItemDef {
            sockets,
            gem,
            reforgeable,
            socket_views: Vec::new(),
            name: item_name.ok_or_else(|| self.at(node, format!("item `{id}` needs a `name`")))?,
            base,
            id,
            model,
            rarity: Arc::default(),
            kind,
            lore,
            slot,
            stats,
            skills: Vec::new(),
        };
        Ok(ItemFile { item, rarity, uses, stat_at })
    }

    /// `sockets 2` or `sockets "red" "any"`.
    fn sockets(&self, node: &KdlNode) -> R<Vec<String>> {
        self.shape(node, usize::MAX, &[], false)?;
        let first = self.arg(node, 0, "a count or socket types, like `sockets 2` or `sockets \"red\" \"any\"`")?;
        if let KdlValue::Integer(_) = first.value() {
            if let Some(extra) = args(node).nth(1) {
                return Err(self.at_entry(extra, "too many values for `sockets`"));
            }
            return Ok(vec![super::forge::ANY_SOCKET.to_string(); self.whole(first)? as usize]);
        }
        args(node).map(|entry| self.socket_type(entry)).collect()
    }

    fn socket_type(&self, entry: &KdlEntry) -> R<String> {
        let value = self.string(entry)?;
        if value.is_empty() || !value.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_') {
            return Err(self.at_entry(entry, format!("socket type `{value}` may only use a-z, 0-9 and _")));
        }
        Ok(value)
    }

    fn stat_name(&self, entry: &KdlEntry, stat: &str) -> R<()> {
        if stat.is_empty() || !stat.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_') {
            return Err(self.at_entry(entry, format!("stat `{stat}` may only use a-z, 0-9, - and _")));
        }
        Ok(())
    }

    /// `stats { magic-power 50 }`, kebab-case or snake_case. Returns the
    /// stats and where each one is, so undeclared custom stats can be reported.
    fn stats(&self, node: &KdlNode) -> R<(Stats, Vec<(String, String)>)> {
        self.shape(node, 0, &[], true)?;
        let mut stats = Stats::new();
        let mut at = Vec::new();
        for child in children(node) {
            let stat = name(child).replace('-', "_");
            if !stat.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_') {
                return Err(self.at(child, format!("stat `{}` may only use a-z, 0-9, - and _", name(child))));
            }
            let value = self.num_arg(child, "a number")?;
            if stats.insert(stat.clone(), value).is_some() {
                return Err(self.at(child, format!("stat `{}` is listed twice", name(child))));
            }
            at.push((stat, self.position(child)));
        }
        Ok((stats, at))
    }

    /// `skill "id" mana=35 damage=14` (from the library) or
    /// `skill "Name" { ... }` (written inside the item).
    fn skill_use(&self, node: &KdlNode) -> R<SkillUse> {
        let at = self.position(node);
        if node.children().is_some() {
            self.shape(node, 1, &[], true)?;
            let skill_name = self.string(self.arg(node, 0, "a name, like `skill \"Moon Slash\" { ... }`")?)?;
            let def = self.skill(node, skill_name)?;
            return Ok(SkillUse {
                at,
                skill: SkillSource::Inline(Arc::new(def)),
                name: None,
                mana_cost: None,
                cooldown: None,
                chance: None,
                params: Params::new(),
            });
        }
        self.shape(node, 1, &[], false).ok(); // extra args are reported below
        if let Some(extra) = args(node).nth(1) {
            return Err(self.at_entry(extra, "too many values for `skill`"));
        }
        let id = self.id(node, "a library skill id, like `skill \"moon_slash\"`")?;
        let mut u = SkillUse {
            at,
            skill: SkillSource::Library(id),
            name: None,
            mana_cost: None,
            cooldown: None,
            chance: None,
            params: Params::new(),
        };
        for entry in node.entries() {
            let Some(key) = entry.name() else { continue };
            match key.value() {
                "name" => u.name = Some(self.string(entry)?),
                "mana" => u.mana_cost = Some(self.whole(entry)?),
                "cooldown" => u.cooldown = Some(self.number(entry)? as f32),
                "chance" => u.chance = Some(self.chance(entry)?),
                param => {
                    u.params.insert(param.to_string(), self.number(entry)?);
                }
            }
        }
        Ok(u)
    }

    fn chance(&self, entry: &KdlEntry) -> R<f64> {
        let chance = self.number(entry)?;
        if !(0.0..=1.0).contains(&chance) {
            return Err(self.at_entry(entry, "chance must be between 0 and 1"));
        }
        Ok(chance)
    }

    // ---- Skills ------------------------------------------------------------

    fn skill(&self, node: &KdlNode, id: String) -> R<SkillDef> {
        self.shape(node, 1, &[], true)?;
        self.used_params.borrow_mut().clear();
        let mut trigger = None;
        let mut skill = SkillDef {
            name: id.clone(),
            id,
            description: Vec::new(),
            trigger: Trigger::RightClick,
            chance: 1.0,
            mana_cost: 0,
            cooldown: 0.0,
            params: Params::new(),
            conditions: Vec::new(),
            actions: Arc::default(),
            on_fail: Arc::default(),
        };
        let mut actions = Vec::new();

        for child in children(node) {
            match name(child) {
                "name" => skill.name = self.str_arg(child, "a display name")?,
                "description" => {
                    self.shape(child, usize::MAX, &[], false)?;
                    for entry in args(child) {
                        skill.description.push(self.text(entry)?);
                    }
                }
                "trigger" => {
                    self.shape(child, 1, &["every"], false)?;
                    let value = self.string(self.arg(child, 0, "a trigger")?)?;
                    let every = prop(child, "every");
                    let parsed = match value.replace('-', "_").as_str() {
                        "right_click" => Trigger::RightClick,
                        "left_click" => Trigger::LeftClick,
                        "hit" => Trigger::Hit,
                        "damaged" => Trigger::Damaged,
                        "kill" => Trigger::Kill,
                        "sneak" => Trigger::Sneak,
                        "equip" => Trigger::Equip,
                        "unequip" => Trigger::Unequip,
                        "timer" => {
                            let entry = every.ok_or_else(|| self.at(child, "`trigger \"timer\"` needs `every=`, like every=\"5s\""))?;
                            let Num::Lit(ticks) = self.ticks(entry)? else {
                                return Err(self.at_entry(entry, "`every=` must be a fixed time, not a formula"));
                            };
                            if ticks < 20.0 || ticks % 20.0 != 0.0 {
                                return Err(self.at_entry(entry, "timers run in whole seconds, like every=\"5s\""));
                            }
                            Trigger::Timer((ticks / 20.0) as u32)
                        }
                        _ => {
                            return Err(self.at(child, format!(
                                "unknown trigger `{value}` (expected right_click, left_click, hit, damaged, kill, sneak, timer, equip or unequip)"
                            )));
                        }
                    };
                    if let (Some(e), false) = (every, matches!(parsed, Trigger::Timer(_))) {
                        return Err(self.at_entry(e, "`every=` only goes with `trigger \"timer\"`"));
                    }
                    trigger = Some(parsed);
                }
                "param" => {
                    self.shape(child, 2, &[], false)?;
                    let key = self.string(self.arg(child, 0, "a name and a default, like `param damage 10`")?)?;
                    let value = self.number(self.arg(child, 1, "a default value")?)?;
                    skill.params.insert(key, value);
                }
                "chance" => {
                    self.shape(child, 1, &[], false)?;
                    skill.chance = self.chance(self.arg(child, 0, "a chance between 0 and 1")?)?;
                }
                "when" => skill.conditions.push(self.condition(child)?),
                "cost" => {
                    self.shape(child, 0, &["mana"], false)?;
                    skill.mana_cost = self.opt(child, "mana", |e| self.whole(e))?.unwrap_or(0);
                }
                "cooldown" => skill.cooldown = self.num_arg(child, "seconds")? as f32,
                "on-fail" => {
                    self.shape(child, 0, &[], true)?;
                    skill.on_fail = self.actions(children(child), false)?;
                }
                _ => actions.push(self.action(child, false)?),
            }
        }
        skill.trigger = trigger.ok_or_else(|| self.at(node, format!("skill `{}` needs a `trigger`", skill.id)))?;
        skill.actions = Arc::new(actions);

        for (param, offset) in self.used_params.borrow().iter() {
            if !skill.params.contains_key(param) {
                return Err(self.err(*offset, format!("`${param}` is not declared; add `param {param} <default>` to the skill")));
            }
        }
        Ok(skill)
    }

    fn condition(&self, node: &KdlNode) -> R<Condition> {
        let first = self.arg(node, 0, "a condition, like \"sneaking\" or \"!sneaking\"")?;
        let raw = self.string(first)?;
        let (negate, check_name) = match raw.strip_prefix('!') {
            Some(rest) => (true, rest.replace('_', "-")),
            None => (false, raw.replace('_', "-")),
        };
        let check = match check_name.as_str() {
            "sneaking" | "sprinting" | "on-ground" | "in-water" | "burning" | "player" => {
                self.shape(node, 1, &[], false)?;
                match check_name.as_str() {
                    "sneaking" => Check::Sneaking,
                    "sprinting" => Check::Sprinting,
                    "on-ground" => Check::OnGround,
                    "in-water" => Check::InWater,
                    "burning" => Check::Burning,
                    _ => Check::IsPlayer,
                }
            }
            "type" => {
                self.shape(node, 2, &[], false)?;
                let entity = self.string(self.arg(node, 1, "an entity type, like \"zombie\"")?)?;
                Check::Type(entity.trim_start_matches("minecraft:").to_string())
            }
            "health-below" => {
                self.shape(node, 2, &[], false)?;
                Check::HealthBelow(self.number(self.arg(node, 1, "a fraction, like 0.5")?)?)
            }
            other => {
                return Err(self.at_entry(first, format!(
                    "unknown condition `{other}` (expected sneaking, sprinting, on-ground, in-water, burning, player, type or health-below)"
                )));
            }
        };
        Ok(Condition { negate, check })
    }

    /// `on_points`: the subject is a point, so entity-only mechanics are errors.
    fn actions(&self, nodes: &[KdlNode], on_points: bool) -> R<Actions> {
        nodes
            .iter()
            .map(|n| self.action(n, on_points))
            .collect::<R<Vec<_>>>()
            .map(Arc::new)
    }

    fn action(&self, node: &KdlNode, on_points: bool) -> R<Action> {
        let mechanic = match name(node) {
            "target" => return self.target(node, on_points),
            "repeat" => {
                self.shape(node, 1, &["every"], true)?;
                let times = self.num(self.arg(node, 0, "a count, like `repeat 3 every=10 { ... }`")?)?;
                let every = self.opt(node, "every", |e| self.ticks(e))?
                    .ok_or_else(|| self.at(node, "`repeat` needs `every=` (ticks)"))?;
                let actions = self.actions(children(node), on_points)?;
                return Ok(Action::Repeat { times, every, actions });
            }
            "wait" => {
                self.shape(node, 1, &[], false)?;
                return Ok(Action::Wait(self.ticks(self.arg(node, 0, "ticks, like 20 or \"1s\"")?)?));
            }
            "projectile" => {
                self.shape(node, 0, &["speed", "range", "radius"], true)?;
                let (mut trail, mut hit, mut end) = (Arc::default(), Arc::default(), Arc::default());
                for child in children(node) {
                    self.shape(child, 0, &[], true)?;
                    match name(child) {
                        "trail" => trail = self.actions(children(child), true)?,
                        "hit" => hit = self.actions(children(child), false)?,
                        "end" => end = self.actions(children(child), true)?,
                        other => {
                            return Err(self.at(child, format!("unknown projectile block `{other}` (expected trail, hit or end)")));
                        }
                    }
                }
                return Ok(Action::Projectile {
                    speed: self.opt_num(node, "speed", 1.0)?,
                    range: self.opt_num(node, "range", 20.0)?,
                    radius: self.opt_num(node, "radius", 0.6)?,
                    trail,
                    hit,
                    end,
                });
            }
            "chain" => {
                if on_points {
                    return Err(self.at(node, "`chain` jumps between entities, but this block targets points"));
                }
                self.shape(node, 0, &["jumps", "radius", "delay", "beam"], true)?;
                return Ok(Action::Chain {
                    jumps: self.opt_num(node, "jumps", 3.0)?,
                    radius: self.opt_num(node, "radius", 5.0)?,
                    delay: self.opt(node, "delay", |e| self.ticks(e))?.unwrap_or(Num::Lit(4.0)),
                    beam: self.opt(node, "beam", |e| self.string(e))?,
                    actions: self.actions(children(node), false)?,
                });
            }
            "damage" => {
                if let Some(old) = prop(node, "type") {
                    return Err(self.at_entry(old, "`type=` is now `source=` (the Minecraft damage type); for magic/physical damage use `kind=`"));
                }
                self.shape(node, 1, &["kind", "scale", "source", "triggers-hit"], false)?;
                let kind = prop(node, "kind")
                    .map(|e| {
                        let k = self.string(e)?;
                        self.used_kinds.borrow_mut().push((k.clone(), e.span().offset()));
                        Ok(k)
                    })
                    .transpose()?;
                let scale = self.opt_num(node, "scale", 0.0)?;
                if kind.is_none() && let Some(e) = prop(node, "scale") {
                    return Err(self.at_entry(e, "`scale=` needs a `kind=` (its power stat is what gets scaled)"));
                }
                Mechanic::Damage {
                    amount: self.num(self.arg(node, 0, "an amount")?)?,
                    kind,
                    scale,
                    source: self.opt(node, "source", |e| self.string(e))?,
                    triggers_hit: self.opt(node, "triggers-hit", |e| self.boolean(e))?.unwrap_or(false),
                }
            }
            "heal" => {
                self.shape(node, 1, &[], false)?;
                Mechanic::Heal { amount: self.num(self.arg(node, 0, "an amount")?)? }
            }
            "ignite" => {
                self.shape(node, 1, &[], false)?;
                Mechanic::Ignite { seconds: self.num(self.arg(node, 0, "seconds")?)? }
            }
            "glow" => {
                self.shape(node, 1, &[], false)?;
                Mechanic::Glow { seconds: self.num(self.arg(node, 0, "seconds")?)? }
            }
            "knockback" => {
                self.shape(node, 1, &["up"], false)?;
                Mechanic::Knockback {
                    strength: self.num(self.arg(node, 0, "a strength")?)?,
                    up: self.opt_num(node, "up", 0.0)?,
                }
            }
            "effect" => {
                self.shape(node, 1, &["seconds", "level"], false)?;
                Mechanic::Effect {
                    effect: self.string(self.arg(node, 0, "an effect, like \"speed\"")?)?,
                    seconds: self.opt_num(node, "seconds", 5.0)?,
                    level: self.opt_num(node, "level", 1.0)?,
                }
            }
            "particle" => {
                self.shape(node, 1, &["count", "spread", "speed"], false)?;
                Mechanic::Particle {
                    particle: self.string(self.arg(node, 0, "a particle, like \"end_rod\"")?)?,
                    count: self.opt_num(node, "count", 10.0)?,
                    spread: self.opt_num(node, "spread", 0.3)?,
                    speed: self.opt_num(node, "speed", 0.0)?,
                }
            }
            "sound" => {
                self.shape(node, 1, &["volume", "pitch"], false)?;
                Mechanic::Sound {
                    sound: self.string(self.arg(node, 0, "a sound id")?)?,
                    volume: self.opt_num(node, "volume", 1.0)?,
                    pitch: self.opt_num(node, "pitch", 1.0)?,
                }
            }
            "lightning" => {
                self.shape(node, 0, &["effect-only"], false)?;
                Mechanic::Lightning {
                    effect_only: self.opt(node, "effect-only", |e| self.boolean(e))?.unwrap_or(false),
                }
            }
            "message" => Mechanic::Message { text: self.str_arg(node, "text")? },
            "dash" => {
                self.shape(node, 1, &["up"], false)?;
                Mechanic::Dash {
                    strength: self.num(self.arg(node, 0, "a strength")?)?,
                    up: self.opt_num(node, "up", 0.0)?,
                }
            }
            "pull" => {
                self.shape(node, 1, &[], false)?;
                Mechanic::Pull { strength: self.num(self.arg(node, 0, "a strength")?)? }
            }
            "blink" => {
                self.shape(node, 1, &[], false)?;
                Mechanic::Blink { distance: self.num(self.arg(node, 0, "a distance in blocks")?)? }
            }
            other => {
                return Err(self.at(node, format!(
                    "unknown action `{other}` (expected target, repeat, wait, projectile, chain, damage, heal, ignite, glow, knockback, pull, dash, blink, effect, particle, sound, lightning or message)"
                )));
            }
        };
        if on_points && !mechanic.works_on_points() {
            return Err(self.at(node, format!(
                "`{}` needs an entity, but this block targets points; wrap it in a `target \"radius\"` to hit entities near each point",
                mechanic.name()
            )));
        }
        Ok(Action::Mechanic(mechanic))
    }

    fn target(&self, node: &KdlNode, on_points: bool) -> R<Action> {
        let kind_entry = self.arg(node, 0, "a targeter, like \"cone\"")?;
        let kind = self.string(kind_entry)?;
        let options: &[&str] = match kind.as_str() {
            "self" | "victim" | "attacker" => &["required"],
            "ray" | "radius" => &["range", "required"],
            "cone" => &["range", "angle", "required"],
            "forward" => &["distance", "height"],
            "arc" => &["radius", "angle", "points", "height"],
            "line" => &["range", "step"],
            "ring" => &["radius", "points", "height"],
            other => {
                return Err(self.at_entry(kind_entry, format!(
                    "unknown targeter `{other}` (expected self, victim, attacker, ray, radius, cone, forward, arc, line or ring)"
                )));
            }
        };
        self.shape(node, 1, options, true)?;
        let need = |key: &str| {
            self.opt(node, key, |e| self.num(e))?
                .ok_or_else(|| self.at(node, format!("`target \"{kind}\"` needs `{key}=`")))
        };
        let targeter = match kind.as_str() {
            "self" => Targeter::Myself,
            "victim" => Targeter::Victim,
            "attacker" => Targeter::Attacker,
            "ray" => Targeter::Ray { range: self.opt_num(node, "range", 20.0)? },
            "radius" => Targeter::Radius { range: need("range")? },
            "cone" => Targeter::Cone { range: need("range")?, angle: self.opt_num(node, "angle", 90.0)? },
            "forward" => Targeter::Forward {
                distance: self.opt_num(node, "distance", 2.0)?,
                height: self.opt_num(node, "height", 1.2)?,
            },
            "arc" => Targeter::Arc {
                radius: self.opt_num(node, "radius", 2.5)?,
                angle: self.opt_num(node, "angle", 90.0)?,
                points: self.opt_num(node, "points", 9.0)?,
                height: self.opt_num(node, "height", 1.2)?,
            },
            "line" => Targeter::Line {
                range: self.opt_num(node, "range", 10.0)?,
                step: self.opt_num(node, "step", 0.5)?,
            },
            _ => Targeter::Ring {
                radius: self.opt_num(node, "radius", 1.5)?,
                points: self.opt_num(node, "points", 16.0)?,
                height: self.opt_num(node, "height", 0.1)?,
            },
        };
        // `self` keeps whatever the subject already is.
        let points = match targeter {
            Targeter::Myself => on_points,
            ref t => t.gives_points(),
        };
        let mut filter = Vec::new();
        let mut actions = Vec::new();
        for child in children(node) {
            if name(child) == "when" {
                if points {
                    return Err(self.at(child, "`when` checks entities, but this block targets points"));
                }
                filter.push(self.condition(child)?);
            } else {
                actions.push(self.action(child, points)?);
            }
        }
        Ok(Action::Target {
            targeter,
            filter,
            required: self.opt(node, "required", |e| self.boolean(e))?.unwrap_or(false),
            actions: Arc::new(actions),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn err(src: &str) -> String {
        parse_file("test.kdl", src).unwrap_err().to_string()
    }

    fn library() -> BTreeMap<String, Arc<SkillDef>> {
        let mut lib = BTreeMap::new();
        for src in [
            include_str!("../../examples/skills/blades.kdl"),
            include_str!("../../examples/skills/storm.kdl"),
            include_str!("../../examples/skills/starweave.kdl"),
        ] {
            for skill in parse_file("lib", src).unwrap().skills {
                lib.insert(skill.id.clone(), Arc::new(skill));
            }
        }
        lib
    }

    fn rarities() -> Vec<Arc<RarityDef>> {
        let src = include_str!("../../examples/item_rarity.kdl");
        parse_rarities("item_rarity.kdl", src).unwrap().into_iter().map(Arc::new).collect()
    }

    fn item(src: &str) -> ItemDef {
        let mut parsed = parse_file("item", src).unwrap();
        link(parsed.items.remove(0), &library(), &rarities()).unwrap()
    }

    #[test]
    fn rarities_from_file() {
        let list = rarities();
        assert_eq!(list[0].id, "common");
        let epic = list.iter().find(|r| r.id == "epic").unwrap();
        assert_eq!((epic.name.as_str(), epic.color.as_str()), ("Epic", "&5"));

        // Items without a rarity get the first one.
        let mut parsed = parse_file("i.kdl", "item \"a\" {\n  name \"A\"\n  base \"x\"\n}").unwrap();
        assert_eq!(link(parsed.items.remove(0), &library(), &list).unwrap().rarity.id, "common");

        let mut parsed = parse_file("i.kdl", "item \"a\" {\n  name \"A\"\n  base \"x\"\n  rarity \"godly\"\n}").unwrap();
        let problems = link(parsed.items.remove(0), &library(), &list).unwrap_err();
        assert!(problems[0].starts_with("i.kdl:4:3: unknown rarity `godly`"), "{problems:?}");

        let e = parse_rarities("item_rarity.kdl", "rarity \"x\" {\n  name \"X\"\n  colour \"&7\"\n}").unwrap_err().to_string();
        assert!(e.starts_with("item_rarity.kdl:3:3: unknown rarity field `colour`"), "{e}");
        let e = parse_rarities("item_rarity.kdl", "rarity \"x\" {\n  name \"X\"\n}").unwrap_err().to_string();
        assert!(e.contains("needs a `color`"), "{e}");
    }

    #[test]
    fn examples_load_and_link() {
        let moon = item(include_str!("../../examples/items/moonlit_edge.kdl"));
        assert_eq!(moon.stats["damage"], 12.0);
        assert_eq!(moon.slot, Slot::MainHand);
        assert_eq!(moon.skills.len(), 3);
        assert_eq!(moon.skills[0].name, "Moon Slash");
        assert_eq!(moon.skills[1].skill.trigger, Trigger::LeftClick);
        assert_eq!(moon.skills[2].skill.trigger, Trigger::Hit);

        let storm = item(include_str!("../../examples/items/stormcaller.kdl"));
        let thunder = &storm.skills[0];
        assert_eq!(thunder.mana_cost, 35);
        assert_eq!(thunder.params["damage"], 14.0); // overridden by the item
        assert_eq!(thunder.params["range"], 20.0); // default from the skill
        assert!(matches!(thunder.skill.actions[1], Action::Target { required: true, .. }));
        assert_eq!(storm.stats["mana_regen"], 2.0);

        let armor = parse_file("a", include_str!("../../examples/items/starweave.kdl")).unwrap();
        let slots: Vec<Slot> = armor.items.iter().map(|f| f.item.slot).collect();
        assert_eq!(slots, [Slot::Head, Slot::Chest, Slot::Legs, Slot::Feet, Slot::OffHand]);
        assert_eq!(armor.items[1].item.stats["magic_power"], 20.0);
        // magic-power and magic-armor are declared by kinds.kdl (the loader
        // adds each kind's power and defense stats).
        let kinds = parse_kinds("kinds.kdl", include_str!("../../examples/kinds.kdl")).unwrap();
        let declared = kinds.iter().flat_map(|k| k.power.iter().chain(&k.defense).cloned()).collect();
        assert!(armor.items.iter().all(|f| check_stats(f, &declared).is_empty()));
    }

    #[test]
    fn damage_kinds() {
        let kinds = parse_kinds("kinds.kdl", include_str!("../../examples/kinds.kdl")).unwrap();
        assert_eq!(
            kinds[1],
            DamageKind {
                id: "magic".into(),
                power: Some("magic_power".into()),
                defense: Some("magic_armor".into()),
                source: "magic".into(),
                display: KindDisplay { prefix: "✦ ".into(), color: "&d".into() },
            }
        );
        assert_eq!(kinds[0].defense, None);
        assert_eq!(kinds[0].power.as_deref(), Some("strength"));

        let parsed = parse_file("s", include_str!("../../examples/skills/storm.kdl")).unwrap();
        assert!(parsed.kind_refs.iter().all(|(k, _)| k == "magic"));
        let Action::Target { actions, .. } = &parsed.skills[0].actions[1] else { panic!() };
        let Action::Mechanic(Mechanic::Damage { kind, source, .. }) = &actions[1] else { panic!("{actions:?}") };
        assert_eq!((kind.as_deref(), source.as_deref()), (Some("magic"), Some("lightning_bolt")));

        let e = err("skill \"s\" {\n  trigger \"hit\"\n  damage 5 type=\"magic\"\n}");
        assert!(e.contains("`type=` is now `source=`"), "{e}");
        let e = err("skill \"s\" {\n  trigger \"hit\"\n  damage 5 scale=0.5\n}");
        assert!(e.contains("`scale=` needs a `kind=`"), "{e}");
        let e = err("skill \"s\" {\n  trigger \"hit\"\n  damage 5 triggers-hit=1\n}");
        assert!(e.contains("expected #true or #false"), "{e}");
        let e = parse_kinds("kinds.kdl", "kind \"fire\" {\n  powr \"x\"\n}").unwrap_err().to_string();
        assert!(e.starts_with("kinds.kdl:2:3: unknown kind field `powr`"), "{e}");
    }

    #[test]
    fn stats_and_formulas() {
        // Undeclared custom stats (often typos) are reported.
        let parsed = parse_file("t.kdl", "item \"a\" {\n  name \"A\"\n  base \"x\"\n  stats {\n    damge 5\n  }\n}").unwrap();
        let problems = check_stats(&parsed.items[0], &Default::default());
        assert!(problems[0].starts_with("t.kdl:5:5: unknown stat `damge`"), "{problems:?}");

        // Main-hand-only stats and skills on armor.
        let e = err("item \"a\" {\n  name \"A\"\n  base \"minecraft:iron_boots\"\n  stats {\n    damage 5\n  }\n}");
        assert!(e.contains("`damage` only works on main-hand items"), "{e}");

        // Formula errors point inside the formula.
        let e = err("skill \"s\" {\n  trigger \"hit\"\n  damage \"2 * \"\n}");
        assert!(e.starts_with("test.kdl:3:15: in formula: expected a number"), "{e}");
        let e = err("skill \"s\" {\n  trigger \"hit\"\n  description \"Deals <$power>\"\n}");
        assert!(e.contains("`$power` is not declared"), "{e}");
        let e = err("item \"a\" {\n  name \"A\"\n  base \"x\"\n  lore \"<$damage>\"\n}");
        assert!(e.contains("only works in skills"), "{e}");

        // `self.<stat>` uses are collected for the loader to check.
        let parsed = parse_file("t.kdl", include_str!("../../examples/skills/storm.kdl")).unwrap();
        assert!(parsed.stat_refs.iter().any(|(s, _)| s == "magic_power"));
    }

    #[test]
    fn errors_point_at_the_problem() {
        let e = err("skill \"s\" {\n  trigger \"right_click\"\n  particle \"end_rod\" cuont=3\n}");
        assert!(e.starts_with("test.kdl:3:22: unknown option `cuont`"), "{e}");

        let e = err("skill \"s\" {\n  trigger \"hit\"\n  damage $dmg\n}");
        assert!(e.starts_with("test.kdl:3:10: `$dmg` is not declared"), "{e}");

        let e = err("skill \"s\" {\n  trigger \"hit\"\n  target \"arc\" {\n    damage 5\n  }\n}");
        assert!(e.contains("`damage` needs an entity"), "{e}");

        let e = err("item \"a\" {\n  base \"x\"\n}");
        assert!(e.contains("needs a `name`"), "{e}");
    }

    #[test]
    fn link_errors() {
        let mut parsed = parse_file("i.kdl", "item \"a\" {\n  name \"A\"\n  base \"x\"\n  skill \"thunderstrike\" dmage=3\n  skill \"nope\"\n}").unwrap();
        let problems = link(parsed.items.remove(0), &library(), &rarities()).unwrap_err();
        assert!(problems[0].starts_with("i.kdl:4:3: skill `thunderstrike` has no param `dmage`"), "{problems:?}");
        assert!(problems[1].starts_with("i.kdl:5:3: unknown skill `nope`"), "{problems:?}");
    }

    #[test]
    fn lore_layout() {
        let formats = parse_stat_formats("f", include_str!("../../examples/stats_format.kdl")).unwrap().0;
        let lore = item(include_str!("../../examples/items/moonlit_edge.kdl")).lore_lines(&PlayerStats::new(), &formats);
        assert_eq!(lore[0], "&5Epic Sword");
        // Vanilla-backed stats show too, in the file's order.
        let at = |line: &str| lore.iter().position(|l| l == line).unwrap_or_else(|| panic!("{line} in {lore:?}"));
        let at_stat = |name: &str| lore.iter().position(|l| l.contains(name)).unwrap_or_else(|| panic!("{name} in {lore:?}"));
        assert!(at_stat("Damage") < at_stat("Attack Speed"));
        assert_eq!(item(include_str!("../../examples/items/moonlit_edge.kdl")).display_name(), "&5Moonlit Edge");
        assert!(lore.iter().any(|l| l == "&5◆ &fMoon Slash &8· &7Right-click"), "{lore:?}");
        assert!(lore.iter().any(|l| l == "&8  20 mana · 3s cooldown"), "{lore:?}");

        // Placeholders use the holder's stats: 14 + 50 * 0.25 = 26.5.
        let storm = item(include_str!("../../examples/items/stormcaller.kdl"));
        let stats = PlayerStats::from([("magic_power".to_string(), 50.0)]);
        let lore = storm.lore_lines(&stats, &formats).join("\n");
        assert!(lore.contains("&526.5"), "{lore}");
        assert!(lore.contains("Mana Regen"), "{lore}");
    }

    #[test]
    fn lore_entries() {
        let src = "item \"a\" {\n  name \"A\"\n  kind \"Sword\"\n  base \"minecraft:stick\"\n  lore \"Old.\"\n  stats {\n    armor 2\n    health 4\n  }\n}";
        let a = item(src);
        let lore = |file: &str| {
            let formats = StatFormats { layout: parse_stats_layout("f.kdl", file).unwrap().0, ..Default::default() };
            a.lore_lines(&PlayerStats::new(), &formats)
        };
        let default = a.lore_lines(&PlayerStats::new(), &StatFormats::default());
        let (rarity, text, armor, health) = (default[0].as_str(), "&7&oOld.", "&7Armor &b+2", "&7Health &b+4");

        assert_eq!(default, [rarity, "", text, "", armor, health]);
        // Blank lines are only where they are listed, even at the top or doubled.
        assert_eq!(lore("stats\nrarity\nlore"), [armor, health, rarity, text]);
        assert_eq!(lore("empty\nlore\nempty\nempty\nrarity"), ["", text, "", "", rarity]);
        // `stat` places one stat; `stats` is the rest.
        assert_eq!(lore("stat \"health\"\nrarity\nstats"), [health, rarity, armor]);
        assert_eq!(lore("stat \"health\"\nstat \"armor\""), [health, armor]);
        assert_eq!(lore("stat \"speed\"\nempty\nlore"), [text]);
        // No blank line around what this item does not have (no skills here).
        assert_eq!(lore("rarity\nempty\nskills\nempty\nlore"), [rarity, "", text]);
        assert_eq!(lore("lore\nempty\nskills"), [text]);
        assert_eq!(lore("skills\nempty\nlore"), [text]);
        // Left out or switched off: not shown.
        assert_eq!(lore("lore\n/-stats\nrarity"), [text, rarity]);
        assert!(lore("").is_empty());

        let e = parse_stats_layout("f.kdl", "rarity\nstatz").unwrap_err().to_string();
        assert!(e.starts_with("f.kdl:2:1: unknown lore entry `statz`"), "{e}");
        let e = parse_stats_layout("f.kdl", "lore\nlore").unwrap_err().to_string();
        assert!(e.contains("listed twice"), "{e}");
        let e = parse_stats_layout("f.kdl", "stat \"armor\"\nstat \"armor\"").unwrap_err().to_string();
        assert!(e.contains("listed twice"), "{e}");
        let e = parse_stats_layout("f.kdl", "empty \"x\"").unwrap_err().to_string();
        assert!(e.contains("too many values"), "{e}");
        assert!(parse_stats_layout("f.kdl", "stat").is_err());
        let refs = parse_stats_layout("f.kdl", "lore\nstat \"magic-power\"").unwrap().1;
        assert_eq!(refs, [("magic_power".to_string(), "f.kdl:2:1".to_string())]);
    }

    #[test]
    fn dividers_and_fill_line_up() {
        let src = "item \"a\" {\n  name \"A rather long item name here\"\n  base \"minecraft:stick\"\n  stats {\n    armor 2\n    health 40\n    speed 12.5\n  }\n}";
        let a = item(src);
        let mut formats = parse_stat_formats("f.kdl", "format \"&7{name}{fill}&b{sign}{value}\"\nstat \"health\" {\n  format \"&c❤ &7{name}{fill}&c{sign}{value}\"\n}").unwrap().0;
        formats.layout = parse_stats_layout("f.kdl", "divider \"&fStats\" exists=stats\nstats\ndivider exists=stats\nempty\ndivider \"Gems\" exists=sockets\nsockets\nempty\nrarity").unwrap().0;
        let lore = a.lore_lines(&PlayerStats::new(), &formats);
        // No sockets: no divider, and no blank line for it.
        assert_eq!(lore.iter().filter(|l| l.is_empty()).count(), 1, "{lore:?}");
        assert!(!lore.iter().any(|l| l.contains("Gems")), "{lore:?}");
        // Everything that stretches ends up the same width.
        let width = |l: &String| super::tooltip::text_width(l);
        let wanted = width(&lore[0]);
        assert_eq!(lore[..5].iter().map(width).collect::<Vec<_>>(), [wanted; 5], "{lore:?}");
        assert!(wanted >= 150);
        assert!(lore.iter().all(|l| !l.contains(super::tooltip::FILL)), "{lore:?}");

        // A long stat line stretches the rest.
        let long = item("item \"a\" {\n  name \"A\"\n  base \"minecraft:stick\"\n  stats {\n    armor 123456789012345678901234567890\n    health 4\n  }\n}");
        let lore = long.lore_lines(&PlayerStats::new(), &formats);
        assert!(width(&lore[0]) > 150);
        assert_eq!(width(&lore[1]), width(&lore[0]), "{lore:?}");

        let e = parse_stats_layout("f.kdl", "divider exists=socket").unwrap_err().to_string();
        assert!(e.contains("unknown section `socket`"), "{e}");
        let e = parse_stats_layout("f.kdl", "divider color=\"red\"").unwrap_err().to_string();
        assert!(e.contains("not a color"), "{e}");
        let e = parse_stats_layout("f.kdl", "divider width=3").unwrap_err().to_string();
        assert!(e.contains("unknown option `width`"), "{e}");
        let e = parse_stat_formats("f.kdl", "format \"{name}{fill}{value}{fill}\"").unwrap_err().to_string();
        assert!(e.contains("only be used once"), "{e}");
        let parsed = parse_stats_layout("f.kdl", "divider \"x\" color=\"#010203\" fade=\"#a0b0c0\"\ndivider").unwrap().0;
        assert_eq!(parsed.len(), 2);
        assert!(matches!(&parsed[0], LoreEntry::Divider { divider, exists: None } if divider.color == (1, 2, 3) && divider.fade == (0xa0, 0xb0, 0xc0)));
    }

    #[test]
    fn stat_formats() {
        let src = "format \"{name}: {value}\"\nstat \"magic-power\" {\n  name \"Puissance\"\n  format \"&d{sign}{value} {name}\"\n}\nstat \"armor\" {\n  hidden #true\n}\nstat \"speed\"";
        let (formats, refs) = parse_stat_formats("f.kdl", src).unwrap();
        assert_eq!(refs[0], ("magic_power".to_string(), "f.kdl:2:1".to_string()));
        let stats = Stats::from([("armor".into(), 5.0), ("speed".into(), -10.0), ("magic_power".into(), 20.0), ("luck".into(), 1.0)]);
        // Listed order, hidden left out, unlisted (luck) last with the default.
        assert_eq!(formats.lines(&stats), ["&d+20 Puissance", "Speed: -10", "Luck: 1"]);

        let e = parse_stat_formats("f.kdl", "stat \"damage\" {\n  format \"{name} {amount}\"\n}").unwrap_err().to_string();
        assert!(e.starts_with("f.kdl:2:18: unknown placeholder `{amount}`"), "{e}");
        let e = parse_stat_formats("f.kdl", "stat \"damage\" {\n  colour \"&c\"\n}").unwrap_err().to_string();
        assert!(e.contains("unknown stat field `colour`"), "{e}");
        let e = parse_stat_formats("f.kdl", "stat \"damage\"\nstat \"damage\"").unwrap_err().to_string();
        assert!(e.contains("listed twice"), "{e}");
    }

    #[test]
    fn triggers_and_new_mechanics() {
        // Armor can carry skills with non-click triggers.
        let parsed = parse_file("a", include_str!("../../examples/items/starweave.kdl")).unwrap();
        let lib = library();
        let triggers: Vec<Trigger> = parsed
            .items
            .into_iter()
            .map(|f| link(f, &lib, &rarities()).unwrap().skills[0].skill.trigger)
            .collect();
        assert_eq!(
            triggers,
            [Trigger::Timer(5), Trigger::Damaged, Trigger::Kill, Trigger::Sneak, Trigger::Equip]
        );

        // Click and hit skills are rejected on armor.
        let mut f = parse_file("i.kdl", "item \"h\" {
  name \"H\"
  base \"minecraft:iron_helmet\"
  skill \"moon_slash\"
}").unwrap();
        let problems = link(f.items.remove(0), &lib, &rarities()).unwrap_err();
        assert!(problems[0].contains("only works for main-hand items"), "{problems:?}");

        // Projectile and chain shapes.
        let blades = parse_file("b", include_str!("../../examples/skills/blades.kdl")).unwrap();
        let bolt = blades.skills.iter().find(|s| s.id == "moon_bolt").unwrap();
        let Action::Projectile { trail, hit, end, .. } = &bolt.actions[1] else { panic!("{:?}", bolt.actions) };
        assert_eq!((trail.len(), hit.len(), end.len()), (1, 3, 1));
        assert!(matches!(hit[0], Action::Mechanic(Mechanic::Damage { triggers_hit: true, .. })));
        let storm = parse_file("s", include_str!("../../examples/skills/storm.kdl")).unwrap();
        let Action::Target { actions, .. } = &storm.skills[0].actions[1] else { panic!() };
        assert!(actions.iter().any(|a| matches!(a, Action::Chain { beam: Some(b), .. } if b == "electric_spark")));

        // Errors.
        let e = err("skill \"s\" {
  trigger \"timer\"
}");
        assert!(e.contains("needs `every=`"), "{e}");
        let e = err("skill \"s\" {
  trigger \"timer\" every=\"1.5s\"
}");
        assert!(e.contains("whole seconds"), "{e}");
        let e = err("skill \"s\" {
  trigger \"sneak\" every=\"5s\"
}");
        assert!(e.contains("only goes with `trigger \"timer\"`"), "{e}");
        let e = err("skill \"s\" {
  trigger \"hit\"
  projectile {
    trail {
      damage 5
    }
  }
}");
        assert!(e.contains("`damage` needs an entity"), "{e}");
        let e = err("skill \"s\" {
  trigger \"hit\"
  target \"arc\" {
    chain {
    }
  }
}");
        assert!(e.contains("`chain` jumps between entities"), "{e}");
        let e = err("skill \"s\" {
  trigger \"hit\"
  projectile {
    boom {
    }
  }
}");
        assert!(e.contains("unknown projectile block `boom`"), "{e}");
    }
}
