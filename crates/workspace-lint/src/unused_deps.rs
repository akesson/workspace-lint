use crate::config::UnusedDepsConfig;
use crate::diagnostic::Diagnostic;
use crate::diagnostic::builder::at_crate;
use crate::workspace;
use fs_err as fs;
use ignore::WalkBuilder;
use std::collections::{BTreeMap, HashSet};
use std::path::Path;
use syn::visit::Visit;

pub const LINT: &str = "workspace-lint::unused-deps";

pub fn check(config: &UnusedDepsConfig) -> Vec<Diagnostic> {
    let root_toml = fs::read_to_string("Cargo.toml").unwrap_or_else(|e| {
        eprintln!("failed to read root Cargo.toml: {e}");
        std::process::exit(1);
    });

    let root: toml::Value = root_toml.parse().unwrap_or_else(|e| {
        eprintln!("failed to parse root Cargo.toml: {e}");
        std::process::exit(1);
    });

    let member_patterns = workspace::extract_member_patterns(&root);
    let member_dirs = workspace::expand_member_patterns(&member_patterns);

    let mut diagnostics = Vec::new();

    for dir in &member_dirs {
        let cargo_path = dir.join("Cargo.toml");
        if !cargo_path.exists() {
            continue;
        }

        let content = fs::read_to_string(&cargo_path).unwrap_or_else(|e| {
            eprintln!("failed to read {}: {e}", cargo_path.display());
            std::process::exit(1);
        });

        let doc: toml::Value = content.parse().unwrap_or_else(|e| {
            eprintln!("failed to parse {}: {e}", cargo_path.display());
            std::process::exit(1);
        });

        let deps = collect_deps_from_toml(&doc, &config.ignore);

        if deps.is_empty() {
            continue;
        }

        let idents = collect_rs_idents(dir);
        let unused = find_unused_deps(deps, &idents);

        if !unused.is_empty() {
            let n = unused.len();
            let mut builder = at_crate(
                LINT,
                format!(
                    "{n} possibly unused dependenc{} in {}",
                    if n == 1 { "y" } else { "ies" },
                    cargo_path.display()
                ),
                dir.clone(),
            );
            for label in unused {
                builder = builder.help(label);
            }
            diagnostics.push(
                builder
                    .note("build.rs-generated code, *-sys link-only deps, and feature-plumbing-only deps may still cause false positives")
                    .note("verify by removing the dep and running `cargo build --all-targets`")
                    .note("if the build breaks, add the dep to [unused-deps] ignore in your config")
                    .build(),
            );
        }
    }

    diagnostics
}

fn collect_deps_from_toml(doc: &toml::Value, ignore: &[String]) -> BTreeMap<String, Vec<String>> {
    let mut deps: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for section in ["dependencies", "dev-dependencies", "build-dependencies"] {
        if let Some(table) = doc.get(section).and_then(|v| v.as_table()) {
            for name in table.keys() {
                if ignore.iter().any(|i| i == name) {
                    continue;
                }
                let normalized = name.replace('-', "_");
                deps.entry(normalized)
                    .or_default()
                    .push(format!("[{section}] {name}"));
            }
        }
    }
    deps
}

fn find_unused_deps(deps: BTreeMap<String, Vec<String>>, idents: &HashSet<String>) -> Vec<String> {
    deps.into_iter()
        .filter(|(normalized, _)| !idents.contains(normalized))
        .flat_map(|(_, labels)| labels)
        .collect()
}

fn collect_rs_idents(dir: &Path) -> HashSet<String> {
    let mut idents = HashSet::new();

    for entry in WalkBuilder::new(dir)
        .hidden(false)
        .build()
        .filter_map(|e| e.ok())
    {
        let path = entry.path();
        if path.extension().is_some_and(|ext| ext == "rs")
            && let Ok(content) = fs::read_to_string(path)
        {
            idents.extend(extract_referenced_idents(&content));
        }
    }

    idents
}

fn extract_referenced_idents(source: &str) -> HashSet<String> {
    let Ok(file) = syn::parse_file(source) else {
        return HashSet::new();
    };
    let mut visitor = IdentCollector {
        idents: HashSet::new(),
    };
    visitor.visit_file(&file);
    visitor.idents
}

struct IdentCollector {
    idents: HashSet<String>,
}

impl IdentCollector {
    fn add(&mut self, ident: &syn::Ident) {
        let s = ident.to_string();
        if matches!(s.as_str(), "self" | "super" | "crate" | "Self") {
            return;
        }
        self.idents.insert(s);
    }

    fn add_use_tree(&mut self, tree: &syn::UseTree) {
        match tree {
            syn::UseTree::Path(p) => self.add(&p.ident),
            syn::UseTree::Name(n) => self.add(&n.ident),
            syn::UseTree::Rename(r) => self.add(&r.ident),
            syn::UseTree::Glob(_) => {}
            syn::UseTree::Group(g) => {
                for item in &g.items {
                    self.add_use_tree(item);
                }
            }
        }
    }
}

impl<'ast> Visit<'ast> for IdentCollector {
    fn visit_item_use(&mut self, node: &'ast syn::ItemUse) {
        self.add_use_tree(&node.tree);
        syn::visit::visit_item_use(self, node);
    }

    fn visit_item_extern_crate(&mut self, node: &'ast syn::ItemExternCrate) {
        self.add(&node.ident);
        syn::visit::visit_item_extern_crate(self, node);
    }

    fn visit_attribute(&mut self, node: &'ast syn::Attribute) {
        if node.path().is_ident("derive")
            && let Ok(nested) = node.parse_args_with(
                syn::punctuated::Punctuated::<syn::Path, syn::Token![,]>::parse_terminated,
            )
        {
            for path in &nested {
                if let Some(seg) = path.segments.first() {
                    self.add(&seg.ident);
                }
            }
        }
        syn::visit::visit_attribute(self, node);
    }

    fn visit_macro(&mut self, node: &'ast syn::Macro) {
        if let Some(seg) = node.path.segments.first() {
            self.add(&seg.ident);
        }
        syn::visit::visit_macro(self, node);
    }

    fn visit_path(&mut self, node: &'ast syn::Path) {
        if node.segments.len() > 1
            && let Some(seg) = node.segments.first()
        {
            self.add(&seg.ident);
        }
        syn::visit::visit_path(self, node);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    // --- collect_deps_from_toml ---

    #[test]
    fn collect_deps_basic() {
        let doc: toml::Value = r#"
            [dependencies]
            serde = "1"
            tokio = { workspace = true }
        "#
        .parse()
        .unwrap();
        let deps = collect_deps_from_toml(&doc, &[]);
        assert!(deps.contains_key("serde"));
        assert!(deps.contains_key("tokio"));
    }

    #[test]
    fn collect_deps_normalizes_hyphens() {
        let doc: toml::Value = r#"
            [dependencies]
            my-crate = "1"
        "#
        .parse()
        .unwrap();
        let deps = collect_deps_from_toml(&doc, &[]);
        assert!(deps.contains_key("my_crate"));
    }

    #[test]
    fn collect_deps_respects_ignore() {
        let doc: toml::Value = r#"
            [dependencies]
            serde = "1"
            prost = "0.12"
        "#
        .parse()
        .unwrap();
        let deps = collect_deps_from_toml(&doc, &["prost".into()]);
        assert!(deps.contains_key("serde"));
        assert!(!deps.contains_key("prost"));
    }

    #[test]
    fn collect_deps_all_sections() {
        let doc: toml::Value = r#"
            [dependencies]
            a = "1"
            [dev-dependencies]
            b = "1"
            [build-dependencies]
            c = "1"
        "#
        .parse()
        .unwrap();
        let deps = collect_deps_from_toml(&doc, &[]);
        assert_eq!(deps.len(), 3);
        assert!(deps["a"][0].contains("[dependencies]"));
        assert!(deps["b"][0].contains("[dev-dependencies]"));
        assert!(deps["c"][0].contains("[build-dependencies]"));
    }

    // --- extract_referenced_idents ---

    #[test]
    fn extract_use_statement() {
        let idents = extract_referenced_idents("use foo::bar;");
        assert!(idents.contains("foo"));
    }

    #[test]
    fn extract_use_group() {
        let idents = extract_referenced_idents("use foo::{a, b}; use bar;");
        assert!(idents.contains("foo"));
        assert!(idents.contains("bar"));
    }

    #[test]
    fn extract_use_rename_takes_real_name() {
        let idents = extract_referenced_idents("use foo as bar;");
        assert!(idents.contains("foo"));
        assert!(!idents.contains("bar"));
    }

    #[test]
    fn extract_fully_qualified_path() {
        let idents = extract_referenced_idents("fn f() { tokio::spawn(future); }");
        assert!(idents.contains("tokio"));
    }

    #[test]
    fn extract_attribute_path() {
        let idents = extract_referenced_idents("#[tokio::main] fn main() {}");
        assert!(idents.contains("tokio"));
    }

    #[test]
    fn extract_fully_qualified_derive() {
        let idents = extract_referenced_idents("#[derive(serde::Serialize)] struct S;");
        assert!(idents.contains("serde"));
    }

    #[test]
    fn extract_macro_invocation() {
        let idents = extract_referenced_idents("fn f() { serde_json::json!({}); }");
        assert!(idents.contains("serde_json"));
    }

    #[test]
    fn extract_extern_crate() {
        let idents = extract_referenced_idents("extern crate foo;");
        assert!(idents.contains("foo"));
    }

    #[test]
    fn extract_extern_crate_keeps_real_name() {
        let idents = extract_referenced_idents("extern crate foo as bar;");
        assert!(idents.contains("foo"));
        assert!(!idents.contains("bar"));
    }

    #[test]
    fn ignores_comments() {
        let idents = extract_referenced_idents("// use serde::Deserialize;\nfn main() {}");
        assert!(!idents.contains("serde"));
    }

    #[test]
    fn ignores_strings() {
        let idents =
            extract_referenced_idents(r#"fn main() { let _ = "use serde::Deserialize;"; }"#);
        assert!(!idents.contains("serde"));
    }

    #[test]
    fn skips_self_super_crate() {
        let idents = extract_referenced_idents(
            "use crate::a; use super::b; use self::c; fn f() { let _ = crate::x::y; }",
        );
        assert!(!idents.contains("self"));
        assert!(!idents.contains("super"));
        assert!(!idents.contains("crate"));
        assert!(!idents.contains("Self"));
    }

    #[test]
    fn skips_bare_local_idents() {
        let idents = extract_referenced_idents("fn main() { let x = 5; let _ = x; }");
        assert!(!idents.contains("x"));
    }

    #[test]
    fn parse_failure_yields_empty() {
        let idents = extract_referenced_idents("this is @#$ not rust !!!");
        assert!(idents.is_empty());
    }

    // --- find_unused_deps ---

    #[test]
    fn find_unused_all_used() {
        let mut deps = BTreeMap::new();
        deps.insert("serde".into(), vec!["[dependencies] serde".into()]);
        let mut idents = HashSet::new();
        idents.insert("serde".into());
        assert!(find_unused_deps(deps, &idents).is_empty());
    }

    #[test]
    fn find_unused_none_used() {
        let mut deps = BTreeMap::new();
        deps.insert("serde".into(), vec!["[dependencies] serde".into()]);
        let idents = HashSet::new();
        let unused = find_unused_deps(deps, &idents);
        assert_eq!(unused, vec!["[dependencies] serde"]);
    }

    #[test]
    fn find_unused_partial() {
        let mut deps = BTreeMap::new();
        deps.insert("serde".into(), vec!["[dependencies] serde".into()]);
        deps.insert("rand".into(), vec!["[dependencies] rand".into()]);
        let mut idents = HashSet::new();
        idents.insert("serde".into());
        let unused = find_unused_deps(deps, &idents);
        assert_eq!(unused, vec!["[dependencies] rand"]);
    }

    // --- collect_rs_idents (tempdir) ---

    #[test]
    fn collect_rs_idents_finds_files() {
        let tmp = TempDir::new().unwrap();
        let src = tmp.path().join("src");
        std::fs::create_dir(&src).unwrap();
        std::fs::write(src.join("lib.rs"), "use serde::Deserialize;").unwrap();
        std::fs::write(src.join("main.rs"), "fn main() { tokio::spawn(()); }").unwrap();
        std::fs::write(src.join("readme.txt"), "use rand;").unwrap();

        let idents = collect_rs_idents(tmp.path());
        assert!(idents.contains("serde"));
        assert!(idents.contains("tokio"));
        assert!(!idents.contains("rand"));
    }

    #[test]
    fn collect_rs_idents_empty_dir() {
        let tmp = TempDir::new().unwrap();
        let idents = collect_rs_idents(tmp.path());
        assert!(idents.is_empty());
    }
}
