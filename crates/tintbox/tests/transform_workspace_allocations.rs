//! Allocation and output contract for caller-owned transform scratch.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::fs;
use std::hint::black_box;
use std::path::{Path, PathBuf};

use tintbox::format::decode::{TYPE_CMYK_8, TYPE_CMYK_FLT, TYPE_RGB_8, TYPE_RGB_FLT};
use tintbox::opt::OptimizationStrategy;
use tintbox::profile::{Profile, RenderingIntent};
use tintbox::transform::{Transform, TransformWorkspace};

thread_local! {
    static TRACKING: Cell<bool> = const { Cell::new(false) };
    static ALLOCATIONS: Cell<usize> = const { Cell::new(0) };
    static LARGEST_REQUEST: Cell<usize> = const { Cell::new(0) };
}

struct TestAllocator;

fn record_allocation(pointer: *mut u8, size: usize) {
    if !pointer.is_null() && TRACKING.try_with(Cell::get).unwrap_or(false) {
        let _ = ALLOCATIONS.try_with(|count| count.set(count.get() + 1));
        let _ = LARGEST_REQUEST.try_with(|largest| largest.set(largest.get().max(size)));
    }
}

// SAFETY: every operation delegates unchanged to System. The thread-local
// counters allocate no memory and never access the allocated block.
unsafe impl GlobalAlloc for TestAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let pointer = unsafe { System.alloc(layout) };
        record_allocation(pointer, layout.size());
        pointer
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        let pointer = unsafe { System.alloc_zeroed(layout) };
        record_allocation(pointer, layout.size());
        pointer
    }

    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        let pointer = unsafe { System.realloc(pointer, layout, size) };
        record_allocation(pointer, size);
        pointer
    }

    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        unsafe { System.dealloc(pointer, layout) };
    }
}

#[global_allocator]
static ALLOCATOR: TestAllocator = TestAllocator;

#[derive(Debug)]
struct AllocationStats {
    count: usize,
    largest_request: usize,
}

struct Tracking;

impl Drop for Tracking {
    fn drop(&mut self) {
        TRACKING.set(false);
    }
}

fn allocations_during(call: impl FnOnce()) -> AllocationStats {
    ALLOCATIONS.set(0);
    LARGEST_REQUEST.set(0);
    assert!(!TRACKING.replace(true));
    let scope = Tracking;
    call();
    drop(scope);
    AllocationStats {
        count: ALLOCATIONS.get(),
        largest_request: LARGEST_REQUEST.get(),
    }
}

fn testbed_dir() -> PathBuf {
    Path::new(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../vendor/Little-CMS/testbed"
    ))
    .to_path_buf()
}

fn load(name: &str) -> Vec<u8> {
    fs::read(testbed_dir().join(name)).unwrap_or_else(|_| panic!("read {name}"))
}

fn cmyk_input(n_pixels: usize) -> Vec<u8> {
    let mut input = Vec::with_capacity(n_pixels * 4);
    for pixel in 0..n_pixels {
        input.extend_from_slice(&[
            (pixel.wrapping_mul(29).wrapping_add(3) & 0xff) as u8,
            (pixel.wrapping_mul(47).wrapping_add(11) & 0xff) as u8,
            (pixel.wrapping_mul(71).wrapping_add(19) & 0xff) as u8,
            (pixel.wrapping_mul(101).wrapping_add(23) & 0xff) as u8,
        ]);
    }
    input
}

#[test]
fn warmed_workspace_reuses_batched_storage_without_allocating() {
    const PIXELS: usize = 513;
    const SUFFIX: [u8; 3] = [0xa7, 0x5c, 0xe1];

    let input_profile = load("test1.icc");
    let output_profile = load("test3.icc");
    let input = cmyk_input(PIXELS);

    let mut oracle = vec![0u8; PIXELS * 3];
    let oracle_built = tintbox_oracle::do_transform_packed(
        &[&input_profile, &output_profile],
        &[
            RenderingIntent::RelativeColorimetric.to_raw(),
            RenderingIntent::RelativeColorimetric.to_raw(),
        ],
        &[false, false],
        &[1.0, 1.0],
        TYPE_CMYK_8,
        TYPE_RGB_8,
        &input,
        &mut oracle,
        PIXELS,
    );
    assert!(oracle_built, "LittleCMS oracle transform must build");

    let input_parsed = Profile::open(&input_profile).expect("open CMYK input profile");
    let output_parsed = Profile::open(&output_profile).expect("open RGB output profile");
    let transform = Transform::new_simple_with_formats_strategy(
        &input_parsed,
        &output_parsed,
        RenderingIntent::RelativeColorimetric,
        false,
        TYPE_CMYK_8,
        TYPE_RGB_8,
        OptimizationStrategy::AccurateFast,
    )
    .expect("build tintbox transform");
    assert!(
        transform.batched_fired(),
        "fixture must enter the batched path"
    );

    let mut workspace = TransformWorkspace::new();
    let mut warm = vec![0x6d; oracle.len() + SUFFIX.len()];
    warm[oracle.len()..].copy_from_slice(&SUFFIX);
    transform.do_transform_with_workspace(&input, &mut warm, PIXELS, &mut workspace);
    assert_eq!(&warm[..oracle.len()], oracle, "warm output vs oracle");
    assert_eq!(&warm[oracle.len()..], &SUFFIX, "warm output suffix");

    let mut first = vec![0x4b; oracle.len() + SUFFIX.len()];
    let mut second = vec![0xd2; oracle.len() + SUFFIX.len()];
    first[oracle.len()..].copy_from_slice(&SUFFIX);
    second[oracle.len()..].copy_from_slice(&SUFFIX);
    let stats = allocations_during(|| {
        transform.do_transform_with_workspace(
            black_box(&input),
            &mut first,
            PIXELS,
            black_box(&mut workspace),
        );
        transform.do_transform_with_workspace(
            black_box(&input),
            &mut second,
            PIXELS,
            black_box(&mut workspace),
        );
    });

    // Exact color and bounds assertions deliberately precede the allocation
    // assertion so an allocation RED is meaningful only after the oracle passes.
    assert_eq!(&first[..oracle.len()], oracle, "first reuse vs oracle");
    assert_eq!(&second[..oracle.len()], oracle, "second reuse vs oracle");
    assert_eq!(&first[oracle.len()..], &SUFFIX, "first output suffix");
    assert_eq!(&second[oracle.len()..], &SUFFIX, "second output suffix");
    eprintln!("workspace reuse oracle passed; allocation stats: {stats:?}");
    assert_eq!(
        stats.count, 0,
        "a warmed fitting workspace allocated; largest request={} bytes",
        stats.largest_request
    );
}

#[test]
fn cold_workspace_sizes_each_allocation_to_the_actual_pipeline_width() {
    const PIXELS: usize = 513;
    const SUFFIX: [u8; 3] = [0x39, 0xb4, 0x72];

    let input_profile = load("test1.icc");
    let output_profile = load("test3.icc");
    let input = cmyk_input(PIXELS);

    let mut oracle = vec![0u8; PIXELS * 3];
    let oracle_built = tintbox_oracle::do_transform_packed(
        &[&input_profile, &output_profile],
        &[
            RenderingIntent::RelativeColorimetric.to_raw(),
            RenderingIntent::RelativeColorimetric.to_raw(),
        ],
        &[false, false],
        &[1.0, 1.0],
        TYPE_CMYK_8,
        TYPE_RGB_8,
        &input,
        &mut oracle,
        PIXELS,
    );
    assert!(oracle_built, "LittleCMS oracle transform must build");

    let input_parsed = Profile::open(&input_profile).expect("open CMYK input profile");
    let output_parsed = Profile::open(&output_profile).expect("open RGB output profile");
    let transform = Transform::new_simple_with_formats_strategy(
        &input_parsed,
        &output_parsed,
        RenderingIntent::RelativeColorimetric,
        false,
        TYPE_CMYK_8,
        TYPE_RGB_8,
        OptimizationStrategy::AccurateFast,
    )
    .expect("build tintbox transform");
    assert!(
        transform.batched_fired(),
        "fixture must enter the batched path"
    );

    let pipeline = transform.lut();
    let stages = pipeline.stages();
    let first = stages.first().expect("fixture pipeline is nonempty");
    let last = stages.last().expect("fixture pipeline is nonempty");
    assert_eq!(pipeline.input_channels(), first.input_channels());
    assert_eq!(pipeline.output_channels(), last.output_channels());
    let maximum_width = std::iter::once(pipeline.input_channels())
        .chain(std::iter::once(pipeline.output_channels()))
        .chain(
            stages
                .iter()
                .flat_map(|stage| [stage.input_channels(), stage.output_channels()]),
        )
        .max()
        .expect("fixture has an endpoint width");
    assert_eq!(maximum_width, 4, "fixture's actual maximum width");
    let largest_allowed_request = PIXELS
        .checked_mul(maximum_width)
        .and_then(|elements| elements.checked_mul(std::mem::size_of::<f32>()))
        .expect("fixture allocation bound");

    let mut actual = vec![0xc6; oracle.len() + SUFFIX.len()];
    actual[oracle.len()..].copy_from_slice(&SUFFIX);
    let stats = allocations_during(|| {
        let mut workspace = TransformWorkspace::new();
        transform.do_transform_with_workspace(
            black_box(&input),
            &mut actual,
            PIXELS,
            black_box(&mut workspace),
        );
    });

    // The current transform must first prove byte identity and bounds. Only then
    // does this test constrain how the cold workspace sizes its scratch vectors.
    assert_eq!(&actual[..oracle.len()], oracle, "cold output vs oracle");
    assert_eq!(&actual[oracle.len()..], &SUFFIX, "cold output suffix");
    eprintln!(
        "cold workspace oracle passed; maximum_width={maximum_width}, allocation stats: {stats:?}"
    );
    assert!(
        stats.largest_request <= largest_allowed_request,
        "cold workspace allocated {} bytes at once; actual pipeline width permits at most {largest_allowed_request}",
        stats.largest_request
    );
}

struct PreparedCase {
    label: &'static str,
    transform: Transform,
    input: Vec<u8>,
    oracle: Vec<u8>,
    pixels: usize,
}

struct CaseSpec {
    label: &'static str,
    input_name: &'static str,
    output_name: &'static str,
    in_fmt: u32,
    out_fmt: u32,
    pixels: usize,
    input_channels: usize,
    output_bytes_per_pixel: usize,
    float_input: bool,
}

fn run_prepared_case(
    case: &PreparedCase,
    output: &mut [u8],
    suffix: &[u8],
    workspace: &mut TransformWorkspace,
) {
    case.transform
        .do_transform_with_workspace(&case.input, output, case.pixels, workspace);
    assert_eq!(
        &output[..case.oracle.len()],
        case.oracle,
        "{} output vs oracle",
        case.label
    );
    assert_eq!(
        &output[case.oracle.len()..],
        suffix,
        "{} output suffix",
        case.label
    );
}

fn prepare_case(spec: CaseSpec) -> PreparedCase {
    let input_profile = load(spec.input_name);
    let output_profile = load(spec.output_name);
    let mut input = Vec::with_capacity(
        spec.pixels
            * spec.input_channels
            * if spec.float_input {
                std::mem::size_of::<f32>()
            } else {
                1
            },
    );
    for pixel in 0..spec.pixels {
        for channel in 0..spec.input_channels {
            let sample = pixel
                .wrapping_mul(37 + channel * 16)
                .wrapping_add(11 + channel * 7)
                & 0xff;
            if spec.float_input {
                input.extend_from_slice(&(sample as f32 / 255.0).to_le_bytes());
            } else {
                input.push(sample as u8);
            }
        }
    }

    let mut oracle = vec![0u8; spec.pixels * spec.output_bytes_per_pixel];
    assert!(
        tintbox_oracle::do_transform_packed(
            &[&input_profile, &output_profile],
            &[
                RenderingIntent::RelativeColorimetric.to_raw(),
                RenderingIntent::RelativeColorimetric.to_raw(),
            ],
            &[false, false],
            &[1.0, 1.0],
            spec.in_fmt,
            spec.out_fmt,
            &input,
            &mut oracle,
            spec.pixels,
        ),
        "{}: LittleCMS oracle transform must build",
        spec.label
    );

    let input_parsed = Profile::open(&input_profile).expect("open input profile");
    let output_parsed = Profile::open(&output_profile).expect("open output profile");
    let transform = Transform::new_simple_with_formats_strategy(
        &input_parsed,
        &output_parsed,
        RenderingIntent::RelativeColorimetric,
        false,
        spec.in_fmt,
        spec.out_fmt,
        OptimizationStrategy::AccurateFast,
    )
    .expect("build tintbox transform");
    assert!(transform.batched_fired(), "{}: batched fixture", spec.label);
    PreparedCase {
        label: spec.label,
        transform,
        input,
        oracle,
        pixels: spec.pixels,
    }
}

#[test]
fn warmed_workspace_reuses_storage_across_profiles_formats_and_channel_shapes() {
    const SUFFIX: [u8; 2] = [0x1d, 0xe8];
    let cases = [
        prepare_case(CaseSpec {
            label: "CMYK8 to RGB8 contraction",
            input_name: "test1.icc",
            output_name: "test3.icc",
            in_fmt: TYPE_CMYK_8,
            out_fmt: TYPE_RGB_8,
            pixels: 769,
            input_channels: 4,
            output_bytes_per_pixel: 3,
            float_input: false,
        }),
        prepare_case(CaseSpec {
            label: "RGB8 to CMYK8 expansion",
            input_name: "crayons.icc",
            output_name: "test1.icc",
            in_fmt: TYPE_RGB_8,
            out_fmt: TYPE_CMYK_8,
            pixels: 513,
            input_channels: 3,
            output_bytes_per_pixel: 4,
            float_input: false,
        }),
        prepare_case(CaseSpec {
            label: "CMYK float to RGB float",
            input_name: "test1.icc",
            output_name: "test3.icc",
            in_fmt: TYPE_CMYK_FLT,
            out_fmt: TYPE_RGB_FLT,
            pixels: 300,
            input_channels: 4,
            output_bytes_per_pixel: 3 * std::mem::size_of::<f32>(),
            float_input: true,
        }),
    ];
    let mut forward_workspace = TransformWorkspace::new();
    let mut forward_outputs: Vec<Vec<u8>> = cases
        .iter()
        .map(|case| vec![0x53; case.oracle.len() + SUFFIX.len()])
        .collect();
    for (case, output) in cases.iter().zip(&mut forward_outputs) {
        output[case.oracle.len()..].copy_from_slice(&SUFFIX);
        run_prepared_case(case, output, &SUFFIX, &mut forward_workspace);
    }

    let mut reverse_workspace = TransformWorkspace::new();
    let mut reverse_outputs: Vec<Vec<u8>> = cases
        .iter()
        .map(|case| vec![0x91; case.oracle.len() + SUFFIX.len()])
        .collect();
    for index in (0..cases.len()).rev() {
        let case = &cases[index];
        let output = &mut reverse_outputs[index];
        output[case.oracle.len()..].copy_from_slice(&SUFFIX);
        run_prepared_case(case, output, &SUFFIX, &mut reverse_workspace);
    }

    for (case, output) in cases.iter().zip(&mut forward_outputs) {
        output[..case.oracle.len()].fill(0xa9);
    }
    for (case, output) in cases.iter().zip(&mut reverse_outputs) {
        output[..case.oracle.len()].fill(0x34);
    }
    let stats = allocations_during(|| {
        for (case, output) in cases.iter().zip(&mut forward_outputs) {
            run_prepared_case(case, output, &SUFFIX, black_box(&mut forward_workspace));
        }
        for index in (0..cases.len()).rev() {
            run_prepared_case(
                &cases[index],
                &mut reverse_outputs[index],
                &SUFFIX,
                black_box(&mut reverse_workspace),
            );
        }
    });

    eprintln!("both workspace orders passed exact oracles; allocation stats: {stats:?}");
    assert_eq!(
        stats.count, 0,
        "warmed cross-shape workspace allocated; largest request={} bytes",
        stats.largest_request
    );
}
