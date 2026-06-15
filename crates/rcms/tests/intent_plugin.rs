//! Slice-8 T3: wiring of the [`RenderingIntentPlugin`] custom-intent seam through
//! the link dispatcher (`link_icc_intents_in`) and `Transform::new_in`.
//!
//! - **Functional**: register a custom intent (id 10); building a transform that
//!   requests intent 10 runs the plugin's `link` (proven by a sentinel flag),
//!   instead of the builtin `default_icc_intents`.
//! - **Differential**: a plugin whose `link` just delegates to
//!   `default_icc_intents` under id 10 yields a transform whose output is
//!   bit-identical to the builtin `RelativeColorimetric` transform — the seam is
//!   transparent when the plugin re-uses the builtin builder.
//! - **Regression**: with no plugin registered, `new_in` == `new` (builtin path).

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use rcms::context::Context;
use rcms::link::default_icc_intents;
use rcms::pipeline::Pipeline;
use rcms::plugin::RenderingIntentPlugin;
use rcms::profile::{Profile, RenderingIntent};
use rcms::transform::{Flags, Transform};
use rcms::Result;

const CUSTOM_INTENT: u32 = 10;

fn crayons_bytes() -> Vec<u8> {
    let path: PathBuf = Path::new(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../vendor/Little-CMS/testbed/crayons.icc"
    ))
    .to_path_buf();
    std::fs::read(path).expect("read crayons.icc")
}

/// A custom intent (id 10) whose `link` records that it ran, then delegates to the
/// builtin `default_icc_intents` — so its output is identical to the builtin path.
struct DelegatingIntent {
    ran: Arc<AtomicBool>,
}

impl RenderingIntentPlugin for DelegatingIntent {
    fn intent(&self) -> u32 {
        CUSTOM_INTENT
    }
    fn description(&self) -> &str {
        "delegating custom intent (test)"
    }
    fn link(
        &self,
        _ctx: &Context,
        profiles: &[&Profile],
        intents: &[RenderingIntent],
        bpc: &[bool],
        adaptation: &[f64],
        flags: u32,
    ) -> Result<Pipeline> {
        self.ran.store(true, Ordering::SeqCst);
        // The plugin recurses into the builtin builder for the non-custom legs,
        // exactly as lcms2's custom intent functions do — but maps the custom
        // intent number onto RelativeColorimetric so the builtin chain accepts it.
        let mapped: Vec<RenderingIntent> = intents
            .iter()
            .map(|i| {
                if i.to_raw() == CUSTOM_INTENT {
                    RenderingIntent::RelativeColorimetric
                } else {
                    *i
                }
            })
            .collect();
        default_icc_intents(profiles, &mapped, bpc, adaptation, flags)
    }
}

fn run_transform(xform: &Transform) -> Vec<f32> {
    // A small grid of RGB pixels in [0, 1].
    let pts = [0.0f32, 0.25, 0.5, 0.75, 1.0];
    let mut input = Vec::new();
    for &r in &pts {
        for &g in &pts {
            for &b in &pts {
                input.extend_from_slice(&[r, g, b]);
            }
        }
    }
    let n = input.len() / 3;
    let mut out = vec![0.0f32; input.len()];
    xform.do_transform_float(&input, &mut out, n);
    out
}

#[test]
fn custom_intent_link_runs() {
    let bytes = crayons_bytes();
    let p = Profile::open(&bytes).expect("open crayons.icc");
    let profiles = [&p, &p];
    let ran = Arc::new(AtomicBool::new(false));

    let mut ctx = Context::new();
    ctx.register_intent(Arc::new(DelegatingIntent {
        ran: Arc::clone(&ran),
    }));

    let intents = [
        RenderingIntent::Other(CUSTOM_INTENT),
        RenderingIntent::Other(CUSTOM_INTENT),
    ];
    let xform = Transform::new_in(
        &ctx,
        &profiles,
        &intents,
        &[false, false],
        &[1.0, 1.0],
        Flags::NOOPTIMIZE,
    )
    .expect("build custom-intent transform");

    assert!(
        ran.load(Ordering::SeqCst),
        "custom intent plugin's link() must have run"
    );
    // Sanity: the transform actually produces output.
    let out = run_transform(&xform);
    assert!(out.iter().all(|v| v.is_finite()));
}

#[test]
fn custom_intent_delegating_to_default_matches_builtin() {
    let bytes = crayons_bytes();
    let p = Profile::open(&bytes).expect("open crayons.icc");
    let profiles = [&p, &p];

    // Builtin RelativeColorimetric (no plugin).
    let builtin = Transform::new(
        &profiles,
        &[
            RenderingIntent::RelativeColorimetric,
            RenderingIntent::RelativeColorimetric,
        ],
        &[false, false],
        &[1.0, 1.0],
        Flags::NOOPTIMIZE,
    )
    .expect("builtin transform");

    // Custom intent 10 that delegates to default_icc_intents (mapped to RelCol).
    let mut ctx = Context::new();
    ctx.register_intent(Arc::new(DelegatingIntent {
        ran: Arc::new(AtomicBool::new(false)),
    }));
    let custom = Transform::new_in(
        &ctx,
        &profiles,
        &[
            RenderingIntent::Other(CUSTOM_INTENT),
            RenderingIntent::Other(CUSTOM_INTENT),
        ],
        &[false, false],
        &[1.0, 1.0],
        Flags::NOOPTIMIZE,
    )
    .expect("custom transform");

    let a = run_transform(&builtin);
    let b = run_transform(&custom);
    assert_eq!(a.len(), b.len());
    for (i, (x, y)) in a.iter().zip(b.iter()).enumerate() {
        assert_eq!(x.to_bits(), y.to_bits(), "pixel sample {i} differs");
    }
}

#[test]
fn no_plugin_new_in_matches_new() {
    let bytes = crayons_bytes();
    let p = Profile::open(&bytes).expect("open crayons.icc");
    let profiles = [&p, &p];
    let intents = [
        RenderingIntent::RelativeColorimetric,
        RenderingIntent::RelativeColorimetric,
    ];

    let plain = Transform::new(
        &profiles,
        &intents,
        &[false, false],
        &[1.0, 1.0],
        Flags::NOOPTIMIZE,
    )
    .expect("new");
    let ctx = Context::new();
    let in_ = Transform::new_in(
        &ctx,
        &profiles,
        &intents,
        &[false, false],
        &[1.0, 1.0],
        Flags::NOOPTIMIZE,
    )
    .expect("new_in");

    assert_eq!(plain.lut().input_channels, in_.lut().input_channels);
    assert_eq!(plain.lut().output_channels, in_.lut().output_channels);
}
