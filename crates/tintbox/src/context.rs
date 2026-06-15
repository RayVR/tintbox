//! Per-operation context. Replaces lcms2's global Context0 + mutable registries
//! with an explicit, thread-safe-by-construction value. NOT Clone (cloning a
//! context is a deliberate, expensive op once plugin registries arrive in slice
//! 8 — do not set a cheap-clone expectation now). The logger is a lazy `&dyn`,
//! not a captureless `fn`: messages format only when a logger is present.

use std::sync::Arc;

use crate::plugin::{
    InterpolatorFactory, ParametricCurvePlugin, Plugins, RenderingIntentPlugin, TagDescriptor,
    TagTypePlugin,
};
use crate::sig::Signature;

// `Optimizer` lives in `crate::opt` (re-exported by `crate::plugin`); import it
// from there to keep the canonical path.
use crate::opt::Optimizer;

/// Diagnostic sink. `args` is formatted only if a logger is installed, so rich
/// diagnostics cost nothing on the no-logger path and never allocate in Error.
pub trait Logger {
    fn log(&self, error_code: u32, args: &core::fmt::Arguments<'_>);
}

#[derive(Default)]
pub struct Context<'a> {
    logger: Option<&'a dyn Logger>,
    /// The plugin registries (slice-8). Default-empty, so a freshly-built context
    /// behaves exactly as before plugins existed: every dispatcher matches the
    /// builtin path. NOT auto-cloned with the context — cloning a `Context`
    /// remains a deliberate, explicit op (the registries are cheap `Arc` handles,
    /// but the `'a` logger borrow is not).
    plugins: Plugins,
}

impl<'a> Context<'a> {
    pub fn new() -> Self {
        Context::default()
    }
    pub fn with_logger(logger: &'a dyn Logger) -> Self {
        Context {
            logger: Some(logger),
            plugins: Plugins::default(),
        }
    }
    pub fn log(&self, code: u32, args: core::fmt::Arguments<'_>) {
        if let Some(l) = self.logger {
            l.log(code, &args);
        }
    }

    /// The plugin registries this context carries.
    pub fn plugins(&self) -> &Plugins {
        &self.plugins
    }

    /// Mutable access to the plugin registries, for bulk/advanced edits. Prefer
    /// the `register_*` helpers for the common case.
    pub fn plugins_mut(&mut self) -> &mut Plugins {
        &mut self.plugins
    }

    /// Register a custom parametric tone-curve plugin. Register-order is priority
    /// (first match wins); builtins are matched first, so a plugin can only add
    /// NEW function-type ids. Returns `&mut self` for chaining.
    pub fn register_parametric_curve(
        &mut self,
        plugin: Arc<dyn ParametricCurvePlugin>,
    ) -> &mut Self {
        self.plugins.parametric_curves.push(plugin);
        self
    }

    /// Register a custom on-disk tag-type handler. Register-order is priority;
    /// builtins are matched first, so a plugin can only add a NEW type signature.
    pub fn register_tag_type(&mut self, plugin: Arc<dyn TagTypePlugin>) -> &mut Self {
        self.plugins.tag_types.push(plugin);
        self
    }

    /// Register a custom logical tag and the on-disk types it may serialize as.
    pub fn register_tag(&mut self, sig: Signature, descriptor: Arc<TagDescriptor>) -> &mut Self {
        self.plugins.tags.push((sig, descriptor));
        self
    }

    /// Register a custom rendering-intent plugin. Register-order is priority;
    /// builtins are matched first, so a plugin can only add a NEW intent number.
    pub fn register_intent(&mut self, plugin: Arc<dyn RenderingIntentPlugin>) -> &mut Self {
        self.plugins.intents.push(plugin);
        self
    }

    /// Set the custom pipeline optimizer (replaces any previously set). It is
    /// consulted before the builtin strategy chain and may decline (`None`).
    pub fn set_optimizer(&mut self, optimizer: Arc<dyn Optimizer>) -> &mut Self {
        self.plugins.optimizer = Some(optimizer);
        self
    }

    /// Register a custom interpolator factory. Register-order is priority; a
    /// factory may decline (`None`) and fall through to the builtin factory.
    pub fn register_interpolator(&mut self, factory: Arc<dyn InterpolatorFactory>) -> &mut Self {
        self.plugins.interpolators.push(factory);
        self
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
            fn log(&self, code: u32, _args: &core::fmt::Arguments<'_>) {
                self.0.set(code);
            }
        }
        let cap = Cap(Cell::new(0));
        Context::with_logger(&cap).log(42, format_args!("x={}", 7));
        assert_eq!(cap.0.get(), 42);
    }
}
