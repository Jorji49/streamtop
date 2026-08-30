//! streamtop library (CLI binary + integration tests).

#![forbid(unsafe_code)]
#![allow(clippy::too_many_arguments)]
#![allow(clippy::type_complexity)]
// ISO BMFF / MPEG-TS / SCTE-35 parsers use fixed-width big-endian fields; lengths checked first.
#![allow(clippy::cast_possible_truncation)]
#![allow(clippy::cast_precision_loss)]
#![allow(clippy::cast_sign_loss)]
#![allow(clippy::cast_lossless)]
#![allow(clippy::cast_possible_wrap)]
#![allow(clippy::unreadable_literal)]
#![allow(clippy::too_many_lines)]
#![allow(clippy::missing_const_for_fn)]
#![allow(clippy::must_use_candidate)]
#![allow(clippy::doc_markdown)]
#![allow(clippy::struct_excessive_bools)]
#![allow(clippy::missing_errors_doc)] // Public API uses color_eyre::Result; module docs cover failure modes.

pub mod engine;
pub mod models;
pub mod ui;
