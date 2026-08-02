mod config;
mod font;
mod session;

pub mod view;

pub use config::TerminalConfig;
// Quien use la vista necesita hablar de colores: que no tenga que enterarse
// de qué crate viene el tipo.
pub use ghostty_vt::Rgb;
pub use font::{default_terminal_font, default_terminal_font_features};
pub use session::TerminalSession;

#[cfg(test)]
mod tests;
