//! HTML pages for the session browser.
//!
//! Split into small modules: page builders, proc snippets, layout shell, and live client JS.

mod cast;
mod client_js;
mod escape;
mod fleet;
mod format;
mod index;
mod layout;
mod proc;
mod session;
mod session_export;

pub use cast::{cast_player_page, PLAYER_CSS, PLAYER_JS};
pub use index::index_page;
pub use session::session_page;
pub(crate) use session_export::{session_export_page, CastExport};

#[cfg(test)]
mod tests {
  include!("tests.rs");
}
