//! Showplan XML parser, plan model, and layered tree layout for the plan viewer.
//!
//! - [`parse`] turns showplan XML (estimated or actual, `.sqlplan` files) into a [`Plan`].
//! - [`layout`] places a statement's operators SSMS-style (root left, children right).
//! - [`icon_for`] maps operators to Cobalt's icon set; [`OpIcon::category`] tints them.
//! - [`statement_summary`] / [`plan_to_json`] describe a plan for status text and agents.
//!
//! No egui here: everything is plain data so the app, tests, and agents share one model.

pub mod error;
pub mod icons;
pub mod layout;
pub mod model;
pub mod parse;
pub mod summary;

pub use error::PlanError;
pub use icons::{icon_for, IconCategory, OpIcon};
pub use layout::{layout, Edge, Layout, LayoutOptions, Rect};
pub use model::{
    ActualStats, Metric, MissingIndex, Node, Plan, PlanObject, PlanParameter, Statement,
    StatementKind, WaitStat, Warning, WarningKind,
};
pub use parse::parse;
pub use summary::{fmt_cost, fmt_thousands, node_label, plan_to_json, statement_summary};
