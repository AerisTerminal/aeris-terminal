//! Desktop-local presentation support shared by the application and its tests.

pub mod account;
#[path = "chart_chrome.rs"]
pub mod chart_chrome;
pub mod command_registry;
pub mod market_worker;
pub mod services;
pub mod trading;

#[cfg(any(test, feature = "diagnostics"))]
#[path = "desktop/test_support/mod.rs"]
pub mod test_support;
