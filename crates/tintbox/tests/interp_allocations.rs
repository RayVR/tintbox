//! Evaluating a prepared CLUT must not allocate for each input color.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::hint::black_box;
use tintbox::interp::{
    eval_4_inputs, eval_4_inputs_float, eval_n_inputs, eval_n_inputs_float, InterpParams,
};

thread_local! {
    static TRACKING: Cell<bool> = const { Cell::new(false) };
    static ALLOCATIONS: Cell<usize> = const { Cell::new(0) };
}

struct TestAllocator;

fn record_allocation(pointer: *mut u8) {
    if !pointer.is_null() && TRACKING.try_with(Cell::get).unwrap_or(false) {
        let _ = ALLOCATIONS.try_with(|count| count.set(count.get() + 1));
    }
}

// SAFETY: every operation delegates unchanged to System. The thread-local
// integer counters allocate no memory and never access the allocated block.
unsafe impl GlobalAlloc for TestAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let pointer = unsafe { System.alloc(layout) };
        record_allocation(pointer);
        pointer
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        let pointer = unsafe { System.alloc_zeroed(layout) };
        record_allocation(pointer);
        pointer
    }

    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        let pointer = unsafe { System.realloc(pointer, layout, size) };
        record_allocation(pointer);
        pointer
    }

    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        unsafe { System.dealloc(pointer, layout) };
    }
}

#[global_allocator]
static ALLOCATOR: TestAllocator = TestAllocator;

struct Tracking;

impl Drop for Tracking {
    fn drop(&mut self) {
        TRACKING.set(false);
    }
}

fn allocations_during(call: impl FnOnce()) -> usize {
    ALLOCATIONS.set(0);
    assert!(!TRACKING.replace(true));
    let scope = Tracking;
    call();
    drop(scope);
    ALLOCATIONS.get()
}

fn grid(dimensions: usize) -> Vec<u32> {
    let mut grid = vec![2; dimensions];
    // Distinct axis domains catch accidental reuse of the unshifted domain.
    grid[1] = 3;
    grid[dimensions - 1] = 4;
    grid
}

type Kernel16 = fn(&[u16], &mut [u16], &[u16], &InterpParams);
type KernelFloat = fn(&[f32], &mut [f32], &[f32], &InterpParams);

fn check_integer(kernel: Kernel16, dimensions: usize) {
    let grid = grid(dimensions);
    for outputs in [1, 3, 4] {
        let params = InterpParams::new(&grid, dimensions, outputs);
        let nodes = grid.iter().map(|&n| n as usize).product::<usize>();
        let table: Vec<_> = (0..nodes * outputs)
            .map(|i| (i as u32).wrapping_mul(1733).wrapping_add(7919) as u16)
            .collect();
        for endpoint in [None, Some(0), Some(u16::MAX)] {
            let input: Vec<_> = (0..dimensions)
                .map(|i| endpoint.unwrap_or((i as u32 * 8111 + 1297) as u16))
                .collect();
            let expected = tintbox_oracle::interp16(&grid, outputs, &table, 0, &input)
                .expect("independent interpolation oracle");
            let mut actual = vec![0x9abd; outputs + 2];
            let calls = allocations_during(|| {
                kernel(
                    black_box(&input),
                    &mut actual,
                    black_box(&table),
                    black_box(&params),
                );
            });
            assert_eq!(&actual[..outputs], expected, "integer color result");
            assert_eq!(&actual[outputs..], &[0x9abd, 0x9abd], "output suffix");
            eprintln!("integer oracle passed: dimensions={dimensions}, outputs={outputs}, allocations={calls}");
            assert_eq!(
                calls, 0,
                "prepared integer interpolation allocated per color"
            );
        }
    }
}

fn check_float(kernel: KernelFloat, dimensions: usize) {
    let grid = grid(dimensions);
    for outputs in [1, 3, 4] {
        let params = InterpParams::new(&grid, dimensions, outputs);
        let nodes = grid.iter().map(|&n| n as usize).product::<usize>();
        let table: Vec<_> = (0..nodes * outputs)
            .map(|i| ((i as u32).wrapping_mul(1733).wrapping_add(7919) as u16) as f32 / 65535.0)
            .collect();
        for endpoint in [None, Some(0.0), Some(1.0)] {
            let input: Vec<_> = (0..dimensions)
                .map(|i| endpoint.unwrap_or(((i * 71 + 17) % 251) as f32 / 256.0))
                .collect();
            let expected = tintbox_oracle::interp_float(&grid, outputs, &table, 0, &input)
                .expect("independent interpolation oracle");
            let mut actual = vec![0.125f32; outputs + 2];
            let calls = allocations_during(|| {
                kernel(
                    black_box(&input),
                    &mut actual,
                    black_box(&table),
                    black_box(&params),
                );
            });
            for (actual, expected) in actual[..outputs].iter().zip(expected) {
                assert_eq!(actual.to_bits(), expected.to_bits(), "float color result");
            }
            assert_eq!(&actual[outputs..], &[0.125, 0.125], "output suffix");
            eprintln!("float oracle passed: dimensions={dimensions}, outputs={outputs}, allocations={calls}");
            assert_eq!(calls, 0, "prepared float interpolation allocated per color");
        }
    }
}

#[test]
fn four_input_integer_lookup_does_not_allocate() {
    check_integer(eval_4_inputs, 4);
}

#[test]
fn four_input_float_lookup_does_not_allocate() {
    check_float(eval_4_inputs_float, 4);
}

#[test]
fn recursive_integer_lookup_does_not_allocate() {
    for dimensions in [5, 8, 15, 4] {
        check_integer(eval_n_inputs, dimensions);
    }
}

#[test]
fn recursive_float_lookup_does_not_allocate() {
    for dimensions in [5, 8, 15, 4] {
        check_float(eval_n_inputs_float, dimensions);
    }
}
