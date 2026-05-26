use crate::Issue;
use crate::config::{CargoFeatures, UnusedPubConfig};
use fs_err as fs;
use globset::{Glob, GlobSet, GlobSetBuilder};
use protobuf::Message;
use scip::types::symbol_information::Kind;
use scip::types::{Index, SymbolRole};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::Path;
use std::process::Command;
use syn::Visibility;
use syn::visit::Visit;

pub fn check(config: &UnusedPubConfig) -> Vec<Issue> {
    if config.on_ci_only && std::env::var("CI").is_err() {
        return Vec::new();
    }
    let index = load_index(config);
    let mut decl_map = build_declaration_map(&index);
    mark_used_symbols(&index, &mut decl_map);

    // Candidates: not used cross-crate (unused or same-crate only)
    let candidates: Vec<&SymbolEntry> = decl_map.values().filter(|e| !e.used_cross_crate).collect();
    if candidates.is_empty() {
        return Vec::new();
    }

    // Collect files that have candidates, then parse for pub visibility
    let files: HashSet<&str> = candidates
        .iter()
        .map(|e| e.relative_path.as_str())
        .collect();
    let pub_index = collect_pub_items_for_files(&files);

    let kind_filter = parse_kind_filter(&config.kinds);
    let allowlist = build_allowlist(&config.allowlist);
    let exclude_paths = build_path_filter(&config.exclude_paths);

    let mut removal_by_crate: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut tighten_by_crate: BTreeMap<String, Vec<String>> = BTreeMap::new();

    for entry in decl_map.values() {
        // Skip items used cross-crate
        if entry.used_cross_crate {
            continue;
        }

        // Must be pub (confirmed by syn)
        let Some(def_line) = entry.definition_line else {
            continue;
        };
        let item_key = (entry.relative_path.as_str(), def_line);
        if !pub_index.pub_items.contains(&item_key) {
            continue;
        }

        // Filter: items with common derive macros (likely proc-macro generated refs)
        if pub_index.derive_suppressed.contains(&item_key) {
            continue;
        }

        // Filter: test symbols
        if is_test_symbol(entry) {
            continue;
        }

        // Filter: main function
        if is_main_function(entry) {
            continue;
        }

        // Filter: trait impl
        if is_trait_impl(entry) {
            continue;
        }

        // Filter: exclude-crates
        if let Some(crate_name) = extract_crate_name(&entry.symbol)
            && config.exclude_crates.iter().any(|c| c == crate_name)
        {
            continue;
        }

        // Filter: exclude-paths
        if let Some(ref gs) = exclude_paths
            && gs.is_match(&entry.relative_path)
        {
            continue;
        }

        // Filter: allowlist
        if let Some(ref gs) = allowlist
            && gs.is_match(&entry.display_name)
        {
            continue;
        }

        // Filter: kinds
        if let Some(ref filter) = kind_filter
            && !filter.contains(&entry.kind)
        {
            continue;
        }

        let crate_name = extract_crate_name(&entry.symbol)
            .unwrap_or("unknown")
            .to_string();

        let line_display = def_line + 1; // convert 0-based to 1-based for display
        let kind_str = format_kind(entry.kind);
        let item = format!(
            "{kind_str} `{}` ({}:{})",
            entry.display_name, entry.relative_path, line_display
        );
        if entry.used_same_crate {
            tighten_by_crate.entry(crate_name).or_default().push(item);
        } else {
            removal_by_crate.entry(crate_name).or_default().push(item);
        }
    }

    let note =
        "Note: #[cfg]-gated items, proc-macro usage, and re-exports may cause false positives.";
    let mut issues: Vec<Issue> = Vec::new();

    // Removal candidates first — items with no observed references at all.
    for (crate_name, items) in removal_by_crate {
        let n = items.len();
        let mut details = items;
        details.push(String::new());
        details.push(note.into());
        issues.push(Issue {
            title: format!(
                "Unused pub items in crate `{crate_name}` — {n} removal candidate{} (appears unused, consider removing)",
                if n == 1 { "" } else { "s" }
            ),
            details,
        });
    }

    // Then visibility-tightening candidates — items only used within the same crate.
    for (crate_name, items) in tighten_by_crate {
        let n = items.len();
        let mut details = items;
        details.push(String::new());
        details.push(note.into());
        issues.push(Issue {
            title: format!(
                "Unused pub items in crate `{crate_name}` — {n} item{} only used within same crate (consider `pub(crate)`)",
                if n == 1 { "" } else { "s" }
            ),
            details,
        });
    }

    issues
}

fn load_index(config: &UnusedPubConfig) -> Index {
    let scip_path = if let Some(ref path) = config.scip_index {
        path.clone()
    } else {
        // Write temporary rust-analyzer config for cargo features
        let _temp_config = write_ra_config(&config.cargo_features);

        eprintln!("Running `rust-analyzer scip .` to generate SCIP index...");
        let status = Command::new("rust-analyzer")
            .args(["scip", "."])
            .status()
            .unwrap_or_else(|e| {
                eprintln!("failed to run rust-analyzer: {e}");
                std::process::exit(1);
            });
        if !status.success() {
            eprintln!("rust-analyzer scip failed with {status}");
            std::process::exit(1);
        }
        "index.scip".to_string()
    };

    let bytes = fs::read(&scip_path).unwrap_or_else(|e| {
        eprintln!("failed to read SCIP index at {scip_path}: {e}");
        std::process::exit(1);
    });

    Index::parse_from_bytes(&bytes).unwrap_or_else(|e| {
        eprintln!("failed to parse SCIP index: {e}");
        std::process::exit(1);
    })
}

/// Write a temporary `rust-analyzer.json` to configure cargo features.
/// Returns an Option that, if the file was created and didn't exist before,
/// will remove it when dropped (via TempRaConfig).
fn write_ra_config(features: &CargoFeatures) -> Option<TempRaConfig> {
    let path = Path::new("rust-analyzer.json");
    if path.exists() {
        // Don't overwrite user's existing config
        return None;
    }

    let features_json = match features {
        CargoFeatures::Keyword(kw) if kw == "default" => return None,
        CargoFeatures::Keyword(kw) => format!("\"{kw}\""),
        CargoFeatures::List(list) => {
            let items: Vec<String> = list.iter().map(|f| format!("\"{f}\"")).collect();
            format!("[{}]", items.join(", "))
        }
    };

    let content = format!("{{\"cargo\": {{\"features\": {features_json}}}}}");
    fs::write(path, &content).unwrap_or_else(|e| {
        eprintln!("warning: failed to write rust-analyzer.json: {e}");
    });

    Some(TempRaConfig)
}

/// Guard that removes `rust-analyzer.json` when dropped.
struct TempRaConfig;

impl Drop for TempRaConfig {
    fn drop(&mut self) {
        let _ = std::fs::remove_file("rust-analyzer.json");
    }
}

pub(super) struct SymbolEntry {
    pub(super) symbol: String,
    pub(super) kind: Kind,
    pub(super) display_name: String,
    pub(super) relative_path: String,
    pub(super) is_used: bool,
    pub(super) used_same_crate: bool,
    pub(super) used_cross_crate: bool,
    pub(super) definition_line: Option<i32>,
    pub(super) def_symbol_roles: i32,
    pub(super) is_impl: bool,
}

pub(super) fn build_declaration_map(index: &Index) -> HashMap<String, SymbolEntry> {
    let mut map = HashMap::new();

    for doc in &index.documents {
        // Pre-index definition occurrences by symbol for O(1) lookup
        let mut def_occs: HashMap<&str, &scip::types::Occurrence> = HashMap::new();
        for occ in &doc.occurrences {
            if (occ.symbol_roles & SymbolRole::Definition as i32) != 0 {
                def_occs.insert(&occ.symbol, occ);
            }
        }

        for sym in &doc.symbols {
            if sym.symbol.is_empty() || sym.symbol.starts_with("local ") {
                continue;
            }

            let kind = sym.kind.enum_value().unwrap_or(Kind::UnspecifiedKind);

            // Look up definition occurrence from pre-built index
            let (definition_line, def_symbol_roles) = match def_occs.get(sym.symbol.as_str()) {
                Some(occ) => {
                    let line = if occ.range.is_empty() {
                        None
                    } else {
                        Some(occ.range[0])
                    };
                    (line, occ.symbol_roles)
                }
                None => (None, 0),
            };

            let is_impl = sym.relationships.iter().any(|r| r.is_implementation);

            let new_entry = SymbolEntry {
                symbol: sym.symbol.clone(),
                kind,
                display_name: sym.display_name.clone(),
                relative_path: doc.relative_path.clone(),
                is_used: false,
                used_same_crate: false,
                used_cross_crate: false,
                definition_line,
                def_symbol_roles,
                is_impl,
            };

            // Prefer entries that have a known definition line
            match map.entry(sym.symbol.clone()) {
                std::collections::hash_map::Entry::Vacant(e) => {
                    e.insert(new_entry);
                }
                std::collections::hash_map::Entry::Occupied(mut e) => {
                    if e.get().definition_line.is_none() && new_entry.definition_line.is_some() {
                        e.insert(new_entry);
                    }
                }
            }
        }
    }

    map
}

pub(super) fn mark_used_symbols(index: &Index, decl_map: &mut HashMap<String, SymbolEntry>) {
    for doc in &index.documents {
        let doc_crate = extract_crate_name_from_doc(doc);

        for occ in &doc.occurrences {
            if occ.symbol.is_empty() || occ.symbol.starts_with("local ") {
                continue;
            }
            if occ.symbol_roles != SymbolRole::Definition as i32
                && let Some(entry) = decl_map.get_mut(occ.symbol.as_str())
            {
                entry.is_used = true;

                let entry_crate = extract_crate_name(&entry.symbol);
                if doc_crate.is_some() && doc_crate == entry_crate {
                    entry.used_same_crate = true;
                } else {
                    entry.used_cross_crate = true;
                }
            }
        }
    }
}

/// Extract crate name from a document by inspecting its symbols.
fn extract_crate_name_from_doc(doc: &scip::types::Document) -> Option<&str> {
    doc.symbols
        .iter()
        .find_map(|sym| extract_crate_name(&sym.symbol))
}

struct PubItemsIndex<'a> {
    pub_items: HashSet<(&'a str, i32)>,
    derive_suppressed: HashSet<(&'a str, i32)>,
}

fn collect_pub_items_for_files<'a>(files: &HashSet<&'a str>) -> PubItemsIndex<'a> {
    let mut pub_items = HashSet::new();
    let mut derive_suppressed = HashSet::new();

    for &relative_path in files {
        let path = Path::new(relative_path);
        let Ok(source) = fs::read_to_string(path) else {
            continue;
        };

        let result = collect_pub_items_from_source(&source);
        for line in &result.lines {
            pub_items.insert((relative_path, *line));
        }
        for &line in &result.derive_suppressed_lines {
            derive_suppressed.insert((relative_path, line));
        }
    }

    PubItemsIndex {
        pub_items,
        derive_suppressed,
    }
}

pub(super) struct PubItemsResult {
    pub(super) lines: Vec<i32>,
    pub(super) derive_suppressed_lines: HashSet<i32>,
}

/// Parse Rust source and return 0-based line numbers of pub items.
pub(super) fn collect_pub_items_from_source(source: &str) -> PubItemsResult {
    let Ok(file) = syn::parse_file(source) else {
        return PubItemsResult {
            lines: Vec::new(),
            derive_suppressed_lines: HashSet::new(),
        };
    };

    let mut visitor = PubItemVisitor {
        lines: Vec::new(),
        derive_suppressed_lines: HashSet::new(),
    };
    visitor.visit_file(&file);
    PubItemsResult {
        lines: visitor.lines,
        derive_suppressed_lines: visitor.derive_suppressed_lines,
    }
}

struct PubItemVisitor {
    lines: Vec<i32>,
    derive_suppressed_lines: HashSet<i32>,
}

const SUPPRESSING_DERIVES: &[&str] = &[
    "Serialize",
    "Deserialize",
    "Hash",
    "PartialEq",
    "Eq",
    "PartialOrd",
    "Ord",
    "Clone",
    "Copy",
];

impl PubItemVisitor {
    fn check_vis(&mut self, vis: &Visibility, keyword: &dyn syn::spanned::Spanned) {
        if matches!(vis, Visibility::Public(_)) {
            let line = keyword.span().start().line as i32 - 1; // syn is 1-based, SCIP is 0-based
            self.lines.push(line);
        }
    }

    fn has_suppressing_derive(&self, attrs: &[syn::Attribute]) -> bool {
        attrs.iter().any(|attr| {
            if !attr.path().is_ident("derive") {
                return false;
            }
            let Ok(nested) = attr.parse_args_with(
                syn::punctuated::Punctuated::<syn::Path, syn::Token![,]>::parse_terminated,
            ) else {
                return false;
            };
            nested.iter().any(|path| {
                let ident = path.segments.last().map(|s| s.ident.to_string());
                ident
                    .as_deref()
                    .is_some_and(|name| SUPPRESSING_DERIVES.contains(&name))
            })
        })
    }

    fn check_vis_with_derives(
        &mut self,
        vis: &Visibility,
        keyword: &dyn syn::spanned::Spanned,
        attrs: &[syn::Attribute],
    ) {
        if matches!(vis, Visibility::Public(_)) {
            let line = keyword.span().start().line as i32 - 1;
            self.lines.push(line);
            if self.has_suppressing_derive(attrs) {
                self.derive_suppressed_lines.insert(line);
            }
        }
    }
}

impl<'ast> Visit<'ast> for PubItemVisitor {
    fn visit_item_fn(&mut self, node: &'ast syn::ItemFn) {
        self.check_vis(&node.vis, &node.sig.fn_token);
        syn::visit::visit_item_fn(self, node);
    }

    fn visit_item_struct(&mut self, node: &'ast syn::ItemStruct) {
        self.check_vis_with_derives(&node.vis, &node.struct_token, &node.attrs);
        // Track pub fields
        if let syn::Fields::Named(ref fields) = node.fields {
            for field in &fields.named {
                if let Some(ref ident) = field.ident {
                    self.check_vis(&field.vis, ident);
                }
            }
        }
        syn::visit::visit_item_struct(self, node);
    }

    fn visit_item_enum(&mut self, node: &'ast syn::ItemEnum) {
        self.check_vis_with_derives(&node.vis, &node.enum_token, &node.attrs);
        syn::visit::visit_item_enum(self, node);
    }

    fn visit_item_const(&mut self, node: &'ast syn::ItemConst) {
        self.check_vis(&node.vis, &node.const_token);
        syn::visit::visit_item_const(self, node);
    }

    fn visit_item_static(&mut self, node: &'ast syn::ItemStatic) {
        self.check_vis(&node.vis, &node.static_token);
        syn::visit::visit_item_static(self, node);
    }

    fn visit_item_trait(&mut self, node: &'ast syn::ItemTrait) {
        self.check_vis(&node.vis, &node.trait_token);
        syn::visit::visit_item_trait(self, node);
    }

    fn visit_item_type(&mut self, node: &'ast syn::ItemType) {
        self.check_vis(&node.vis, &node.type_token);
        syn::visit::visit_item_type(self, node);
    }

    fn visit_item_mod(&mut self, node: &'ast syn::ItemMod) {
        self.check_vis(&node.vis, &node.mod_token);
        syn::visit::visit_item_mod(self, node);
    }

    fn visit_impl_item_fn(&mut self, node: &'ast syn::ImplItemFn) {
        self.check_vis(&node.vis, &node.sig.fn_token);
        syn::visit::visit_impl_item_fn(self, node);
    }

    fn visit_impl_item_const(&mut self, node: &'ast syn::ImplItemConst) {
        self.check_vis(&node.vis, &node.const_token);
        syn::visit::visit_impl_item_const(self, node);
    }

    fn visit_impl_item_type(&mut self, node: &'ast syn::ImplItemType) {
        self.check_vis(&node.vis, &node.type_token);
        syn::visit::visit_impl_item_type(self, node);
    }

    fn visit_item_use(&mut self, node: &'ast syn::ItemUse) {
        self.check_vis(&node.vis, &node.use_token);
        syn::visit::visit_item_use(self, node);
    }
}

pub(super) fn is_test_symbol(entry: &SymbolEntry) -> bool {
    (entry.def_symbol_roles & SymbolRole::Test as i32) != 0
}

pub(super) fn is_main_function(entry: &SymbolEntry) -> bool {
    entry.display_name == "main" && entry.kind == Kind::Function
}

pub(super) fn is_trait_impl(entry: &SymbolEntry) -> bool {
    entry.is_impl
}

pub(super) fn extract_crate_name(symbol: &str) -> Option<&str> {
    // SCIP symbol format: "rust-analyzer cargo <crate-name> <version> <descriptors>..."
    let parts: Vec<&str> = symbol.split(' ').collect();
    if parts.len() >= 4 && parts[0] == "rust-analyzer" && parts[1] == "cargo" {
        Some(parts[2])
    } else {
        None
    }
}

pub(super) fn parse_kind_filter(kinds: &[String]) -> Option<HashSet<Kind>> {
    if kinds.is_empty() {
        return None;
    }

    let mut set = HashSet::new();
    for kind_str in kinds {
        match kind_str.to_lowercase().as_str() {
            "function" => {
                set.insert(Kind::Function);
            }
            "method" => {
                set.insert(Kind::Method);
                set.insert(Kind::StaticMethod);
            }
            "struct" => {
                set.insert(Kind::Struct);
            }
            "enum" => {
                set.insert(Kind::Enum);
            }
            "constant" | "const" => {
                set.insert(Kind::Constant);
            }
            "trait" => {
                set.insert(Kind::Trait);
            }
            "type" | "type_alias" => {
                set.insert(Kind::TypeAlias);
            }
            "module" | "mod" => {
                set.insert(Kind::Module);
            }
            "static" => {
                set.insert(Kind::StaticVariable);
            }
            "macro" => {
                set.insert(Kind::Macro);
            }
            "field" | "property" => {
                set.insert(Kind::Property);
            }
            "variant" | "enum_member" => {
                set.insert(Kind::EnumMember);
            }
            other => {
                eprintln!("warning: unknown kind filter `{other}`, ignoring");
            }
        }
    }

    Some(set)
}

pub(super) fn build_path_filter(patterns: &[String]) -> Option<GlobSet> {
    if patterns.is_empty() {
        return None;
    }

    let mut builder = GlobSetBuilder::new();
    for pattern in patterns {
        builder.add(Glob::new(pattern).unwrap_or_else(|e| {
            eprintln!("warning: invalid exclude-paths glob `{pattern}`: {e}");
            std::process::exit(1);
        }));
    }

    Some(builder.build().unwrap_or_else(|e| {
        eprintln!("failed to build exclude-paths filter: {e}");
        std::process::exit(1);
    }))
}

pub(super) fn build_allowlist(patterns: &[String]) -> Option<GlobSet> {
    if patterns.is_empty() {
        return None;
    }

    let mut builder = GlobSetBuilder::new();
    for pattern in patterns {
        builder.add(Glob::new(pattern).unwrap_or_else(|e| {
            eprintln!("warning: invalid allowlist glob `{pattern}`: {e}");
            std::process::exit(1);
        }));
    }

    Some(builder.build().unwrap_or_else(|e| {
        eprintln!("failed to build allowlist: {e}");
        std::process::exit(1);
    }))
}

fn format_kind(kind: Kind) -> &'static str {
    match kind {
        Kind::Function => "fn",
        Kind::Method | Kind::StaticMethod => "method",
        Kind::Struct => "struct",
        Kind::Enum => "enum",
        Kind::EnumMember => "variant",
        Kind::Constant => "const",
        Kind::Trait => "trait",
        Kind::TypeAlias => "type",
        Kind::Module => "mod",
        Kind::StaticVariable => "static",
        Kind::Macro => "macro",
        Kind::Property => "field",
        _ => "item",
    }
}

#[cfg(test)]
mod tests;
