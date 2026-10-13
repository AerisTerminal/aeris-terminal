//! Safe native user-interface primitives.

use std::io;

/// Displays a blocking native error dialog.
///
/// Non-Windows development targets retain a portable standard-error fallback.
///
/// # Errors
/// Returns an operating-system error when the dialog or fallback cannot be written.
pub fn message_box(title: &str, message: &str) -> io::Result<()> {
    #[cfg(target_os = "windows")]
    {
        crate::windows::message_box(title, message)
    }
    #[cfg(not(target_os = "windows"))]
    {
        use std::io::Write as _;
        writeln!(io::stderr().lock(), "{title}: {message}")
    }
}

/// Displays a blocking native yes/no warning dialog.
///
/// Non-Windows targets fail closed because a standard-error fallback cannot
/// obtain explicit user confirmation.
///
/// # Errors
/// Returns an operating-system error when the dialog cannot be displayed.
pub fn confirm_message_box(title: &str, message: &str) -> io::Result<bool> {
    #[cfg(target_os = "windows")]
    {
        crate::windows::confirm_message_box(title, message)
    }
    #[cfg(not(target_os = "windows"))]
    {
        use std::io::Write as _;
        writeln!(io::stderr().lock(), "{title}: {message}")?;
        Ok(false)
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn api_has_a_safe_callable_signature() {
        let callable: fn(&str, &str) -> std::io::Result<()> = super::message_box;
        let _ = callable;
        let confirm: fn(&str, &str) -> std::io::Result<bool> = super::confirm_message_box;
        let _ = confirm;
    }
}
