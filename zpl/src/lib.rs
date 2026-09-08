#![forbid(unsafe_code)]

//! ZPL II parser and renderer library.

pub mod bc;
mod bc128;
pub mod bc1d;
pub mod bc2d;
pub mod cmd;
pub mod codepage;
pub mod config;
pub mod diag;
pub mod error;
pub mod fonts;
mod geom;
mod gfxdata;
pub mod ir;
pub mod maxicode;
pub mod memory;
pub mod reed_solomon;
pub mod render;
pub mod scanner;
pub mod state;
pub(crate) mod tables;
pub mod textfmt;
