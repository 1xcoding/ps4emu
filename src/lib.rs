//! ps4emu — экспериментальный эмулятор PlayStation 4 на Rust.
//!
//! Компоненты:
//! * [`mem`] — виртуальная память гостя;
//! * [`cpu`] — интерпретатор x86-64 (Jaguar);
//! * [`elf`] — загрузчик ELF/SELF;
//! * [`sys`] — системные вызовы Orbis OS (FreeBSD 9);
//! * [`hle`] — реализация функций системных библиотек PS4;
//! * [`oracle`] — база NID и модулей;
//! * [`emulator`] — оркестратор;
//! * [`gpu`] — минимальный программный растеризатор команд GNM;
//! * [`patch`] — работа с патчами и PKG-образами;
//! * [`debug`] — отладчик.

pub mod config;
pub mod debug;
pub mod gpu;
pub mod patch;
pub mod snapshot;
pub mod cpu;
pub mod elf;
pub mod emulator;
pub mod hle;
pub mod mem;
pub mod oracle;
pub mod sys;
pub mod trace;

pub use config::Config;
pub use emulator::{EmuError, Emulator, RunResult, StopReason};
