//! Diagrams for the Providers & models chapter of `docs/`.
//!
//! Pages: providers-boundary, providers-content, providers-credentials, providers-models, providers-turn.
//!
//! One function per diagram, each naming the source file it reads and failing
//! if that file or the item in it is gone. `super::tools_lsp` is the worked
//! example; the rules for adding one are in this module's parent.

#![allow(unused_imports)]

use std::path::Path;

use anyhow::{Context, Result};

use crate::rust;
use crate::shapes;
use crate::svg::Diagram;
