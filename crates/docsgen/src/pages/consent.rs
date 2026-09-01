//! Diagrams for the Consent & permissions chapter of `docs/`.
//!
//! Pages: consent-axes, consent-egress, consent-exempt, consent-ladder, config-discovery.
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
