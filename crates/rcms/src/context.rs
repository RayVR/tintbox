//! Per-operation context. Replaces lcms2's global Context0 + mutable registries
//! with an explicit, thread-safe-by-construction value. NOT Clone (cloning a
//! context is a deliberate, expensive op once plugin registries arrive in slice
//! 8 — do not set a cheap-clone expectation now). The logger is a lazy `&dyn`,
//! not a captureless `fn`: messages format only when a logger is present.

/// Diagnostic sink. `args` is formatted only if a logger is installed, so rich
/// diagnostics cost nothing on the no-logger path and never allocate in Error.
pub trait Logger {
    fn log(&self, error_code: u32, args: &core::fmt::Arguments<'_>);
}

#[derive(Default)]
pub struct Context<'a> {
    logger: Option<&'a dyn Logger>,
}

impl<'a> Context<'a> {
    pub fn new() -> Self { Context { logger: None } }
    pub fn with_logger(logger: &'a dyn Logger) -> Self { Context { logger: Some(logger) } }
    pub fn log(&self, code: u32, args: core::fmt::Arguments<'_>) {
        if let Some(l) = self.logger { l.log(code, &args); }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::cell::Cell;

    #[test]
    fn default_has_no_logger() {
        Context::new().log(0, format_args!("ignored {}", 1)); // must not panic
    }

    #[test]
    fn logger_receives_lazy_message() {
        struct Cap(Cell<u32>);
        impl Logger for Cap {
            fn log(&self, code: u32, _args: &core::fmt::Arguments<'_>) { self.0.set(code); }
        }
        let cap = Cap(Cell::new(0));
        Context::with_logger(&cap).log(42, format_args!("x={}", 7));
        assert_eq!(cap.0.get(), 42);
    }
}
