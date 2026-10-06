//! Peak heap while decoding hostile inputs, measured by a counting global
//! allocator. One test, so no other test allocates concurrently.

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering::Relaxed};
use toolpath_otel::{OtelError, decode_input};

struct Counting;

static NOW: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);

fn grew(n: usize) {
    PEAK.fetch_max(NOW.fetch_add(n, Relaxed) + n, Relaxed);
}

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let p = unsafe { System.alloc(layout) };
        if !p.is_null() {
            grew(layout.size());
        }
        p
    }

    unsafe fn dealloc(&self, p: *mut u8, layout: Layout) {
        unsafe { System.dealloc(p, layout) };
        NOW.fetch_sub(layout.size(), Relaxed);
    }

    unsafe fn realloc(&self, p: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        let q = unsafe { System.realloc(p, layout, new_size) };
        if !q.is_null() {
            if new_size >= layout.size() {
                grew(new_size - layout.size());
            } else {
                NOW.fetch_sub(layout.size() - new_size, Relaxed);
            }
        }
        q
    }
}

#[global_allocator]
static ALLOCATOR: Counting = Counting;

/// Heap allocated above the starting level while `f` runs, at its highest.
fn peak_during<T>(f: impl FnOnce() -> T) -> (T, usize) {
    let base = NOW.load(Relaxed);
    PEAK.store(base, Relaxed);
    let out = f();
    (out, PEAK.load(Relaxed) - base)
}

const MAX_ENTRIES: usize = 1 << 20;
const MIB: usize = 1 << 20;

fn refused(what: &str, bytes: &[u8], name: Option<&str>, check: fn(&OtelError) -> bool) {
    let (r, peak) = peak_during(|| decode_input(bytes, name).map(|v| v.len()));
    let err = r.unwrap_err();
    assert!(check(&err), "{what}: {err:?}");
    assert!(
        peak < MIB,
        "{what}: {} bytes of input peaked at {peak} bytes of heap",
        bytes.len()
    );
}

#[test]
fn hostile_inputs_are_refused_without_building_them() {
    // Four zero bytes are an empty Collector frame: the frames are not
    // collected before the first one is refused.
    refused(
        "64 MiB of zeros",
        &vec![0u8; 64 * MIB],
        None,
        |e| matches!(e, OtelError::Framing(m) if m == "frame 0 is empty"),
    );

    let mut json = br#"{"resourceSpans":["#.to_vec();
    json.extend(b"{},".repeat(MAX_ENTRIES - 1));
    json.extend(br#"{}]}"#);
    refused("an empty-resource JSON bomb", &json, None, |e| {
        matches!(e, OtelError::TooManyEntries { .. })
    });

    #[cfg(feature = "protobuf")]
    refused(
        "an empty-resource protobuf bomb",
        &[0x0a, 0x00].repeat(MAX_ENTRIES + 1),
        Some("bomb.binpb"),
        |e| matches!(e, OtelError::TooManyEntries { .. }),
    );
}
