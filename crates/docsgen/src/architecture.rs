//! The crate graph, read from the workspace manifests at generation time.
//!
//! Members come from the workspace manifest; edges come from each member's
//! `[dependencies]` and `[dev-dependencies]`. A crate added to the workspace
//! appears in the diagram without anybody placing it.
//!
//! # Why not `cargo metadata`
//!
//! It resolves the full dependency tree, every registry crate included, and
//! the diagram shows this workspace's own edges. Reading the `emma-` lines out
//! of each manifest also keeps the generator independent of cargo's output
//! format.

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::{Context, Result};

use crate::svg::{Diagram, Edge, Node, Weight};

/// One workspace member and what it depends on inside the workspace.
pub struct Crate {
    /// The path as the workspace names it: `crates/emma`, `tools/fs`.
    pub path: String,
    /// The package name: `emma`, `emma-tools-fs`.
    pub name: String,
    /// Workspace-internal dependencies, by package name.
    pub deps: Vec<String>,
    /// Workspace-internal dev-dependencies not already in `deps`.
    pub dev_deps: Vec<String>,
}

/// Read every workspace member and its internal edges.
pub fn read(root: &Path) -> Result<Vec<Crate>> {
    let ws = std::fs::read_to_string(root.join("Cargo.toml"))
        .context("the workspace manifest must be readable")?;
    let mut out = Vec::new();
    for path in members(&ws) {
        // The generator is a member of the workspace it documents, and a box
        // for it would describe the documentation rather than Emma.
        if path.ends_with("/docsgen") {
            continue;
        }
        let manifest = root.join(&path).join("Cargo.toml");
        let text = std::fs::read_to_string(&manifest)
            .with_context(|| format!("{} is a workspace member", manifest.display()))?;
        let name = package_name(&text)
            .with_context(|| format!("{} has no [package] name", manifest.display()))?;
        let deps = internal(&text, "[dependencies]");
        let dev: Vec<String> = internal(&text, "[dev-dependencies]")
            .into_iter()
            .filter(|d| !deps.contains(d))
            .collect();
        out.push(Crate {
            path,
            name,
            deps,
            dev_deps: dev,
        });
    }
    Ok(out)
}

fn members(ws: &str) -> Vec<String> {
    let Some(rest) = ws.split("members = [").nth(1) else {
        return Vec::new();
    };
    let Some(list) = rest.split(']').next() else {
        return Vec::new();
    };
    list.lines()
        .filter_map(|l| l.trim().strip_prefix('"'))
        .filter_map(|l| l.split('"').next())
        .map(str::to_string)
        .collect()
}

fn package_name(manifest: &str) -> Option<String> {
    manifest
        .lines()
        .skip_while(|l| l.trim() != "[package]")
        .skip(1)
        .take_while(|l| !l.trim_start().starts_with('['))
        .find_map(|l| {
            let (k, v) = l.split_once('=')?;
            (k.trim() == "name").then(|| v.trim().trim_matches('"').to_string())
        })
}

/// Workspace-internal dependency names under one table heading.
///
/// A dependency is internal when its name starts with `emma`. That is a
/// convention rather than something the manifest states, and it holds for
/// every current member. A workspace crate named otherwise would be missed.
fn internal(manifest: &str, heading: &str) -> Vec<String> {
    manifest
        .lines()
        .skip_while(|l| l.trim() != heading)
        .skip(1)
        .take_while(|l| !l.trim_start().starts_with('['))
        .filter_map(|l| {
            let l = l.trim();
            if l.starts_with('#') {
                return None;
            }
            let name = l.split(['.', ' ', '=']).next()?;
            name.starts_with("emma").then(|| name.to_string())
        })
        .collect()
}

/// Lay the crates out in dependency layers: nothing points upward.
pub fn diagram(crates: &[Crate]) -> Diagram {
    let by_name: BTreeMap<&str, &Crate> = crates.iter().map(|c| (c.name.as_str(), c)).collect();

    // Longest path to a leaf, so a crate sits above everything it depends on.
    // Computed, so a new crate does not need a row chosen for it.
    let mut depth: BTreeMap<&str, usize> = BTreeMap::new();
    for _ in 0..crates.len() {
        for c in crates {
            let d = c
                .deps
                .iter()
                .filter_map(|d| depth.get(d.as_str()).copied())
                .max()
                .map(|m| m + 1)
                .unwrap_or(0);
            depth.insert(c.name.as_str(), d);
        }
    }
    let deepest = depth.values().copied().max().unwrap_or(0);

    let mut layers: Vec<Vec<Node>> = (0..=deepest).map(|_| Vec::new()).collect();
    for c in crates {
        let d = depth[c.name.as_str()];
        // Inverted, so the crate depending on everything is at the top.
        layers[deepest - d].push(Node {
            id: c.name.clone(),
            label: c.path.clone(),
            weight: if c.path == "crates/emma" {
                Weight::Accent
            } else {
                Weight::Plain
            },
        });
    }

    let mut edges = Vec::new();
    for c in crates {
        for d in &c.deps {
            if by_name.contains_key(d.as_str()) {
                edges.push(Edge {
                    from: c.name.clone(),
                    to: d.clone(),
                    weight: Weight::Plain,
                    label: String::new(),
                });
            }
        }
        for d in &c.dev_deps {
            if by_name.contains_key(d.as_str()) {
                edges.push(Edge {
                    from: c.name.clone(),
                    to: d.clone(),
                    weight: Weight::Dim,
                    label: "dev".into(),
                });
            }
        }
    }

    let n = crates.len();
    Diagram {
        prefix: "arch".into(),
        caption: format!(
            "{n} workspace crates in dependency layers. An arrow points from a \
             crate to something it depends on; nothing points upward. Dashed \
             edges are dev-dependencies, which exist only when the tests are \
             built."
        ),
        layers,
        edges,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn root() -> std::path::PathBuf {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .canonicalize()
            .expect("the workspace root is two levels up from this crate")
    }

    /// **If this breaks:** a crate vanishes from the picture while remaining
    /// in the build, because the manifest was only partly read. Checked
    /// against the real workspace rather than a fixture, which would agree
    /// with whoever wrote it.
    #[test]
    fn every_member_is_read_from_the_real_workspace() {
        let crates = read(&root()).expect("the workspace parses");
        let ws = std::fs::read_to_string(root().join("Cargo.toml")).expect("manifest");
        let declared = members(&ws).len() - 1; // docsgen documents, and is not documented
        assert_eq!(
            crates.len(),
            declared,
            "read {} of {declared} members: {:?}",
            crates.len(),
            crates.iter().map(|c| &c.path).collect::<Vec<_>>()
        );
        assert!(
            crates.iter().all(|c| !c.name.is_empty()),
            "a member parsed with no package name"
        );
    }

    /// **If this breaks:** the edges are empty and the picture is eight
    /// unconnected boxes -- which still renders, and still looks like a
    /// diagram.
    #[test]
    fn the_real_workspace_has_the_edges_its_manifests_declare() {
        let crates = read(&root()).expect("the workspace parses");
        let emma = crates
            .iter()
            .find(|c| c.path == "crates/emma")
            .expect("crates/emma is a member");
        assert!(
            emma.deps.len() >= 4,
            "crates/emma depends on most of the workspace: {:?}",
            emma.deps
        );
        assert!(
            crates.iter().any(|c| !c.dev_deps.is_empty()),
            "no dev-dependency edge was found anywhere, and llm has one"
        );
    }

    /// **If this breaks:** a crate is drawn above something that depends on
    /// it, and the arrows read backwards.
    #[test]
    fn nothing_is_drawn_above_a_crate_that_depends_on_it() {
        let crates = read(&root()).expect("the workspace parses");
        let d = diagram(&crates);
        let row = |name: &str| {
            d.layers
                .iter()
                .position(|l| l.iter().any(|n| n.id == name))
                .unwrap_or_else(|| panic!("{name} is somewhere"))
        };
        for c in &crates {
            for dep in &c.deps {
                if crates.iter().any(|x| &x.name == dep) {
                    assert!(
                        row(&c.name) < row(dep),
                        "{} depends on {dep} and is not above it",
                        c.name
                    );
                }
            }
        }
    }

    /// **If this breaks:** a comment naming a crate is read as a dependency.
    /// Several manifests here explain an `emma-` dependency in a comment above
    /// it.
    #[test]
    fn a_commented_out_dependency_is_not_an_edge() {
        let m = "[package]\nname = \"x\"\n\n[dependencies]\n# emma-llm is not used here\nemma-harness.workspace = true\n";
        assert_eq!(internal(m, "[dependencies]"), vec!["emma-harness"]);
    }
}
