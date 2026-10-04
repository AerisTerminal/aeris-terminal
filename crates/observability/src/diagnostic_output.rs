//! Operator diagnostics on standard error.

use std::fmt;
use std::io::{self, Write};

/// Writes one line of operator diagnostics to standard error, like `eprintln!`, but never panics.
///
/// `eprintln!` panics when standard error cannot be written, for example once the pipe the
/// process was launched with has closed. On the desktop's GPUI main thread that panic happens
/// inside a window-procedure callback that cannot unwind, so the process aborts; on a runtime
/// worker it ends the worker. Standard error is the only sink for these lines, so a failed write
/// is dropped instead.
#[macro_export]
macro_rules! diagnostic {
    ($($argument:tt)+) => {
        $crate::write_diagnostic(::std::format_args!($($argument)+))
    };
}

#[doc(hidden)]
pub fn write_diagnostic(arguments: fmt::Arguments<'_>) {
    write_diagnostic_line(&mut io::stderr().lock(), arguments);
}

fn write_diagnostic_line(output: &mut impl Write, arguments: fmt::Arguments<'_>) {
    // Nothing can report a failed diagnostic write; dropping it is the only safe outcome.
    let _ = output.write_fmt(format_args!("{arguments}\n"));
}

#[cfg(test)]
mod tests {
    use super::*;

    struct ClosedPipe;

    impl Write for ClosedPipe {
        fn write(&mut self, _: &[u8]) -> io::Result<usize> {
            Err(io::Error::from(io::ErrorKind::BrokenPipe))
        }

        fn flush(&mut self) -> io::Result<()> {
            Err(io::Error::from(io::ErrorKind::BrokenPipe))
        }
    }

    #[test]
    fn a_line_is_written_with_its_terminator() {
        let mut output = Vec::new();
        write_diagnostic_line(&mut output, format_args!("Aeris {} ready", "market"));
        assert_eq!(output, b"Aeris market ready\n");
    }

    #[test]
    fn a_closed_output_drops_the_line_without_panicking() {
        write_diagnostic_line(&mut ClosedPipe, format_args!("Aeris shutdown failed"));
    }
}
