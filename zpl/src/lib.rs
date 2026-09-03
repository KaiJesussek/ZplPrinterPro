#![forbid(unsafe_code)]

//! ZPL II parser and renderer library.

pub mod bc;
pub mod bc1d;
mod bc128;
pub mod cmd;
pub mod codepage;
pub mod reed_solomon;
pub(crate) mod tables;
pub mod config;
pub mod diag;
pub mod error;
pub mod fonts;
pub mod ir;
mod geom;
mod gfxdata;
pub mod memory;
pub mod render;
pub mod scanner;
pub mod state;
pub mod textfmt;
