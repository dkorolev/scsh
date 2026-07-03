//! HTML pages for the session browser.
//!
//! Split into small modules: page builders, proc snippets, layout shell, and live client JS.

mod cast;
mod client_js;
mod escape;
mod format;
mod index;
mod layout;
mod proc;
mod session;

pub use cast::{cast_player_page, PLAYER_CSS, PLAYER_JS};
pub use index::index_page;
pub use session::session_page;

#[cfg(test)]
mod tests {
  include!("tests.rs");
}
